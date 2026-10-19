//! Redis implementation of the bounded entity substrate, independent of layers.

use async_trait::async_trait;
use redis::aio::ConnectionManager;
use uuid::Uuid;

use super::{
    EntityError, Result,
    backend::{
        CommitOutcome, EntityBackend, EntityPlan, MAX_KEY_BYTES, MAX_TRANSACTION_BYTES,
        MAX_TRANSACTION_ROWS, RowSnapshot, RowWrite, TransactionAuthority, validate_key,
        validate_keys, validate_plan, validate_scan,
    },
    codec::MAX_ROW_BYTES,
    keys::EntityKeys,
};

const READ_LUA: &str = r#"
local max_key, max_row, max_total = tonumber(ARGV[1]), tonumber(ARGV[2]), tonumber(ARGV[3])
local total = 0
for _, key in ipairs(KEYS) do
    local kind = redis.call('TYPE', key).ok
    if kind ~= 'none' and kind ~= 'string' then
        return redis.error_reply('ENTITY_CORRUPT invalid row type')
    end
    local size = redis.call('STRLEN', key)
    total = total + #key + size
    if #key > max_key or size > max_row or total > max_total then
        return redis.error_reply('ENTITY_INVALID read exceeds byte bound')
    end
end
local rows = {}
for i, key in ipairs(KEYS) do rows[i] = redis.call('GET', key) end
return {redis.call('TIME'), rows}
"#;

// The first seven args carry small counts and split temporal bounds. Full IDs
// and timestamps never pass through Lua's floating point integer conversion.
// All key types, predicates, operations, counters and bounds are validated
// before the first mutating command: Lua errors cannot roll back a prior SET.
const TRANSACTION_LUA: &str = r#"
local authorities, checks, writes = tonumber(ARGV[1]), tonumber(ARGV[2]), tonumber(ARGV[3])
if not authorities or not checks or not writes or authorities < 1 or checks < 0 or writes < 0
    or authorities % 1 ~= 0 or checks % 1 ~= 0 or writes % 1 ~= 0
    or authorities + checks + writes > 4096
    or #KEYS ~= authorities + checks + writes
    or #ARGV ~= 9 + 2 * (authorities + checks + writes) then
    return redis.error_reply('ENTITY_INVALID malformed transaction')
end
local function bound(seconds_text, nanos_text)
    if seconds_text == '' and nanos_text == '' then return nil, nil end
    local seconds, nanos = tonumber(seconds_text), tonumber(nanos_text)
    if not seconds or seconds < 0 or seconds > 9223372036 or seconds % 1 ~= 0
        or not nanos or nanos < 0 or nanos >= 1000000000 or nanos % 1 ~= 0
        or (seconds == 9223372036 and nanos > 854775807) then
        return nil, nil, 'ENTITY_INVALID invalid temporal bound'
    end
    return seconds, nanos
end
local lower_s, lower_ns, lower_error = bound(ARGV[4], ARGV[5])
local upper_s, upper_ns, upper_error = bound(ARGV[6], ARGV[7])
if lower_error or upper_error then return redis.error_reply(lower_error or upper_error) end
if lower_s and upper_s and (lower_s > upper_s or (lower_s == upper_s and lower_ns >= upper_ns)) then
    return redis.error_reply('ENTITY_INVALID invalid temporal interval')
end
local prefix, mode = ARGV[8], ARGV[9]
if mode ~= 'commit' and mode ~= 'allocate' then
    return redis.error_reply('ENTITY_INVALID invalid transaction mode')
end
if mode == 'allocate' and (checks ~= 0 or writes ~= 1) then
    return redis.error_reply('ENTITY_INVALID invalid allocation plan')
end
local total, total_stored = 0, 0
for _, key in ipairs(KEYS) do
    if #key == 0 or #key > 1024 or string.sub(key, 1, #prefix) ~= prefix then
        return redis.error_reply('ENTITY_INVALID invalid entity key')
    end
    local kind = redis.call('TYPE', key).ok
    if kind ~= 'none' and kind ~= 'string' then
        return redis.error_reply('ENTITY_CORRUPT invalid row type')
    end
    local size = redis.call('STRLEN', key)
    total_stored = total_stored + #key + size
    if size > 4194304 or total_stored > 16777216 then
        return redis.error_reply('ENTITY_INVALID stored rows exceed byte bound')
    end
    total = total + #key
end
local cursor = 10
local expected = {}
for i = 1, authorities + checks do
    local present, value = ARGV[cursor], ARGV[cursor + 1]
    if present ~= '0' and present ~= '1' then
        return redis.error_reply('ENTITY_INVALID invalid condition flag')
    end
    if #value > 4194304 then return redis.error_reply('ENTITY_INVALID row exceeds byte bound') end
    local marker = present .. value
    if expected[KEYS[i]] and expected[KEYS[i]] ~= marker then
        return redis.error_reply('ENTITY_INVALID contradictory predicates')
    end
    expected[KEYS[i]] = marker
    total = total + #value
    cursor = cursor + 2
end
local seen = {}
for i = 1, writes do
    local key, operation, value = KEYS[authorities + checks + i], ARGV[cursor], ARGV[cursor + 1]
    if seen[key] or #value > 4194304 then
        return redis.error_reply('ENTITY_INVALID invalid or duplicate write')
    end
    seen[key] = true
    if mode == 'commit' and operation ~= 'put' and operation ~= 'delete' then
        return redis.error_reply('ENTITY_INVALID invalid write operation')
    end
    if mode == 'allocate' and (operation ~= 'allocate' or expected[key]) then
        return redis.error_reply('ENTITY_INVALID allocation collides with authority')
    end
    total = total + #value
    cursor = cursor + 2
end
if total > 16777216 then return redis.error_reply('ENTITY_INVALID transaction exceeds byte bound') end
local now = redis.call('TIME')
local now_s, now_ns = tonumber(now[1]), tonumber(now[2]) * 1000
if (lower_s and (now_s < lower_s or (now_s == lower_s and now_ns < lower_ns)))
    or (upper_s and (now_s > upper_s or (now_s == upper_s and now_ns >= upper_ns))) then
    return {-1, ''}
end
cursor = 10
for i = 1, authorities + checks do
    local present, value, current = ARGV[cursor], ARGV[cursor + 1], redis.call('GET', KEYS[i])
    if (present == '0' and current) or (present == '1' and current ~= value) then
        return {0, ''}
    end
    cursor = cursor + 2
end
if mode == 'allocate' then
    local counter = redis.call('GET', KEYS[#KEYS])
    if not counter or not string.match(counter, '^%d+$') or (#counter > 1 and string.sub(counter, 1, 1) == '0')
        or #counter > 19 or (#counter == 19 and counter > '9223372036854775807') then
        return redis.error_reply('ENTITY_CORRUPT invalid or uninitialized counter')
    end
    if counter == '9223372036854775807' then return {3, ''} end
    redis.call('INCR', KEYS[#KEYS])
    -- INCR's Lua reply rounds large integers. GET returns the exact decimal.
    return {2, redis.call('GET', KEYS[#KEYS])}
end
for i = 1, writes do
    local key, operation, value = KEYS[authorities + checks + i], ARGV[cursor], ARGV[cursor + 1]
    if operation == 'put' then redis.call('SET', key, value) else redis.call('DEL', key) end
    cursor = cursor + 2
end
return {1, ''}
"#;

// No maintained global key index. SCAN runs inside one bounded script so the
// response is complete or fails closed. Parent-row validation is still needed
// before committing a decision made from this observation. A large unrelated
// database may exceed this prototype's scan-work bound and is unsupported.
const SCAN_LUA: &str = r#"
local prefix, pattern, limit = ARGV[1], ARGV[2], tonumber(ARGV[3])
local cursor, calls, total, seen, rows = '0', 0, 0, {}, {}
repeat
    calls = calls + 1
    if calls > 2048 then return redis.error_reply('ENTITY_INVALID scan exceeds work bound') end
    local page = redis.call('SCAN', cursor, 'MATCH', pattern, 'COUNT', 128)
    cursor = page[1]
    for _, key in ipairs(page[2]) do
        if string.sub(key, 1, #prefix) ~= prefix or #key > 1024 then
            return redis.error_reply('ENTITY_INVALID invalid scanned key')
        end
        if not seen[key] then
            if #rows >= limit then return redis.error_reply('ENTITY_INVALID scan exceeds row bound') end
            seen[key] = true
            if redis.call('TYPE', key).ok ~= 'string' then
                return redis.error_reply('ENTITY_CORRUPT invalid scanned row type')
            end
            local size = redis.call('STRLEN', key)
            total = total + #key + size
            if size > 4194304 or total > 16777216 then
                return redis.error_reply('ENTITY_INVALID scan exceeds byte bound')
            end
            rows[#rows + 1] = {key, redis.call('GET', key)}
        end
    end
until cursor == '0'
return rows
"#;

#[derive(Clone)]
pub struct RedisEntityBackend {
    connection: ConnectionManager,
    prefix: Vec<u8>,
}

impl RedisEntityBackend {
    pub async fn connect(url: &str, volume_id: Uuid) -> Result<Self> {
        let client = redis::Client::open(url).map_err(backend_error)?;
        let connection = ConnectionManager::new(client)
            .await
            .map_err(backend_error)?;
        let prefix = EntityKeys::new(volume_id, Uuid::nil())
            .volume_prefix()
            .to_vec();
        Ok(Self { connection, prefix })
    }

    fn validate_scope(&self, key: &[u8]) -> Result<()> {
        validate_key(key)?;
        if !key.starts_with(&self.prefix) {
            return Err(EntityError::Invalid(
                "entity key belongs to another volume".into(),
            ));
        }
        Ok(())
    }

    async fn transaction(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
        counter: Option<&[u8]>,
    ) -> Result<(i64, Vec<u8>)> {
        validate_plan(authority, plan)?;
        for check in authority.checks.iter().chain(&plan.checks) {
            self.validate_scope(&check.key)?;
        }
        for write in &plan.writes {
            self.validate_scope(write.key())?;
        }
        if let Some(counter) = counter {
            self.validate_scope(counter)?;
            if authority.checks.len() >= MAX_TRANSACTION_ROWS
                || authority.checks.iter().any(|check| check.key == counter)
            {
                return Err(EntityError::Invalid("invalid allocator authority".into()));
            }
        }
        let script = redis::Script::new(TRANSACTION_LUA);
        let mut invocation = script.prepare_invoke();
        for check in authority.checks.iter().chain(&plan.checks) {
            invocation.key(&check.key);
        }
        for write in &plan.writes {
            invocation.key(write.key());
        }
        if let Some(counter) = counter {
            invocation.key(counter);
        }
        invocation
            .arg(authority.checks.len())
            .arg(plan.checks.len())
            .arg(plan.writes.len() + usize::from(counter.is_some()));
        for bound in [authority.not_before_ns, authority.expires_before_ns] {
            if let Some(bound) = bound {
                invocation
                    .arg(bound / 1_000_000_000)
                    .arg(bound % 1_000_000_000);
            } else {
                invocation.arg("").arg("");
            }
        }
        invocation.arg(&self.prefix).arg(if counter.is_some() {
            "allocate"
        } else {
            "commit"
        });
        for check in authority.checks.iter().chain(&plan.checks) {
            invocation
                .arg(if check.expected.is_some() { "1" } else { "0" })
                .arg(check.expected.as_deref().unwrap_or_default());
        }
        for write in &plan.writes {
            match write {
                RowWrite::Put { value, .. } => {
                    invocation.arg("put").arg(value);
                }
                RowWrite::Delete { .. } => {
                    invocation.arg("delete").arg("");
                }
            }
        }
        if counter.is_some() {
            invocation.arg("allocate").arg("");
        }
        let mut connection = self.connection.clone();
        invocation
            .invoke_async(&mut connection)
            .await
            .map_err(backend_error)
    }
}

fn backend_error(error: redis::RedisError) -> EntityError {
    let message = error.to_string();
    if message.contains("ENTITY_INVALID") {
        EntityError::Invalid(message)
    } else if message.contains("ENTITY_CORRUPT") {
        EntityError::Corrupt(message)
    } else {
        EntityError::Backend(message)
    }
}

#[async_trait]
impl EntityBackend for RedisEntityBackend {
    async fn read(&self, keys: &[Vec<u8>]) -> Result<RowSnapshot> {
        validate_keys(keys)?;
        for key in keys {
            self.validate_scope(key)?;
        }
        let script = redis::Script::new(READ_LUA);
        let mut invocation = script.prepare_invoke();
        for key in keys {
            invocation.key(key);
        }
        invocation
            .arg(MAX_KEY_BYTES)
            .arg(MAX_ROW_BYTES)
            .arg(MAX_TRANSACTION_BYTES);
        let mut connection = self.connection.clone();
        let (time, rows): ((i64, i64), Vec<Option<Vec<u8>>>) = invocation
            .invoke_async(&mut connection)
            .await
            .map_err(backend_error)?;
        if rows.len() != keys.len() || time.0 < 0 || !(0..1_000_000).contains(&time.1) {
            return Err(EntityError::Corrupt(
                "invalid backend clock/read response".into(),
            ));
        }
        let now_ns = time
            .0
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(time.1 * 1000))
            .ok_or(EntityError::Overflow)?;
        Ok(RowSnapshot { now_ns, rows })
    }

    async fn commit(
        &self,
        authority: &TransactionAuthority,
        plan: &EntityPlan,
    ) -> Result<CommitOutcome> {
        match self.transaction(authority, plan, None).await?.0 {
            -1 => Err(EntityError::Fenced),
            0 => Ok(CommitOutcome::Conflict),
            1 => Ok(CommitOutcome::Committed),
            _ => Err(EntityError::Corrupt("invalid transaction response".into())),
        }
    }

    async fn allocate(&self, authority: &TransactionAuthority, counter_key: &[u8]) -> Result<u64> {
        let (status, bytes) = self
            .transaction(authority, &EntityPlan::default(), Some(counter_key))
            .await?;
        match status {
            -1 => Err(EntityError::Fenced),
            0 => Err(EntityError::Conflict),
            3 => Err(EntityError::Exhausted),
            2 => {
                let value = std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|value| *value > 0 && *value <= i64::MAX as u64)
                    .ok_or_else(|| EntityError::Corrupt("invalid allocation response".into()))?;
                Ok(value)
            }
            _ => Err(EntityError::Corrupt("invalid allocation response".into())),
        }
    }

    async fn scan_prefix(&self, prefix: &[u8], limit: usize) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        validate_scan(prefix, limit)?;
        self.validate_scope(prefix)?;
        let mut pattern = Vec::with_capacity(prefix.len() + 1);
        for byte in prefix {
            if matches!(byte, b'*' | b'?' | b'[' | b']' | b'\\') {
                pattern.push(b'\\');
            }
            pattern.push(*byte);
        }
        pattern.push(b'*');
        let mut connection = self.connection.clone();
        let mut rows: Vec<(Vec<u8>, Vec<u8>)> = redis::Script::new(SCAN_LUA)
            .key(prefix)
            .arg(prefix)
            .arg(pattern)
            .arg(limit)
            .invoke_async(&mut connection)
            .await
            .map_err(backend_error)?;
        rows.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(rows)
    }
}
