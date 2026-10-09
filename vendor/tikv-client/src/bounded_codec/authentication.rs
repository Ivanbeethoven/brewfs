//! Borrow capped locking/2PC responses before prost materializes any vectors.
//! Ordinary uncapped SDK clients do not activate this strict catalog schema.

use super::{limit, malformed, take, varint, Schema};
use tonic::Status;

#[derive(Clone, Copy)]
enum Shape {
    Lock,
    Prewrite,
    Commit,
    Rollback,
    Execution,
    Metrics,
}

pub(super) fn inspect(bytes: &[u8], schema: Schema) -> Result<(), Status> {
    let shape = match schema {
        Schema::PessimisticLock => Shape::Lock,
        Schema::Prewrite => Shape::Prewrite,
        Schema::Commit => Shape::Commit,
        Schema::Rollback => Shape::Rollback,
        _ => return Err(malformed()),
    };
    inspect_shape(bytes, shape, 0, &mut 0)
}

fn inspect_shape(
    mut bytes: &[u8],
    shape: Shape,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), Status> {
    if depth > 2 || *nodes >= 16 {
        return Err(limit());
    }
    *nodes += 1;
    let mut counts = [0u8; 32];
    while !bytes.is_empty() {
        let tag = varint(&mut bytes)?;
        let field = tag >> 3;
        let wire = tag & 7;
        if field == 0 || field >= 32 {
            return Err(limit());
        }
        let count = &mut counts[field as usize];
        *count = count.checked_add(1).ok_or_else(limit)?;
        if *count > 1 {
            return Err(limit());
        }
        // No arbitrary nested error body reaches prost or formatting code.
        if matches!(
            shape,
            Shape::Lock | Shape::Prewrite | Shape::Commit | Shape::Rollback
        ) && matches!(field, 1 | 2)
        {
            if wire != 2 {
                return Err(malformed());
            }
            let length = usize::try_from(varint(&mut bytes)?).map_err(|_| malformed())?;
            take(&mut bytes, length)?;
            return Err(Status::failed_precondition(
                "bounded authentication returned a key or region error",
            ));
        }
        let nested = match (shape, field) {
            (Shape::Lock, 7) | (Shape::Prewrite, 5) | (Shape::Commit, 4) | (Shape::Rollback, 3) => {
                Some(Shape::Execution)
            }
            (Shape::Execution, 1..=4) => Some(Shape::Metrics),
            _ => None,
        };
        if let Some(child) = nested {
            if wire != 2 {
                return Err(malformed());
            }
            let length = usize::try_from(varint(&mut bytes)?).map_err(|_| malformed())?;
            let body = take(&mut bytes, length)?;
            inspect_shape(body, child, depth + 1, nodes)?;
            continue;
        }
        match (shape, field, wire) {
            (Shape::Lock, 5, 2) => {
                let length = usize::try_from(varint(&mut bytes)?).map_err(|_| malformed())?;
                if length > 48 << 10 {
                    return Err(limit());
                }
                take(&mut bytes, length)?;
            }
            (Shape::Lock, 6, 0) => {
                if varint(&mut bytes)? > 1 {
                    return Err(malformed());
                }
            }
            (Shape::Lock, 6, 2) => {
                let length = usize::try_from(varint(&mut bytes)?).map_err(|_| malformed())?;
                if length != 1 {
                    return Err(limit());
                }
                if take(&mut bytes, length)?[0] > 1 {
                    return Err(malformed());
                }
            }
            (Shape::Prewrite, 3 | 4, 0) | (Shape::Commit, 3, 0) | (Shape::Metrics, 1..=31, 0) => {
                varint(&mut bytes)?;
            }
            _ => return Err(limit()),
        }
    }
    if matches!(shape, Shape::Lock) && (counts[5] != 1 || counts[6] != 1) {
        return Err(malformed());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_presence_empty_value_and_absence_are_distinct() {
        assert!(inspect(&[0x2a, 0, 0x32, 1, 0], Schema::PessimisticLock).is_ok());
        assert!(inspect(&[0x2a, 0, 0x32, 1, 1], Schema::PessimisticLock).is_ok());
        assert!(inspect(&[0x2a, 0], Schema::PessimisticLock).is_err());
    }

    #[test]
    fn repeated_values_packed_presence_and_force_results_are_rejected() {
        for bytes in [
            vec![0x2a, 0, 0x2a, 0, 0x32, 1, 0],
            vec![0x2a, 0, 0x32, 2, 0, 0],
            vec![0x2a, 0, 0x32, 1, 0, 0x42, 0],
        ] {
            assert!(inspect(&bytes, Schema::PessimisticLock).is_err());
        }
    }

    #[test]
    fn errors_are_rejected_before_nested_materialization() {
        for schema in [
            Schema::PessimisticLock,
            Schema::Prewrite,
            Schema::Commit,
            Schema::Rollback,
        ] {
            assert!(inspect(&[0x0a, 2, 0x0a, 0], schema).is_err());
            assert!(inspect(&[0x12, 2, 0x0a, 0], schema).is_err());
        }
    }

    #[test]
    fn normal_control_and_execution_metrics_are_bounded() {
        assert!(inspect(
            &[0x18, 1, 0x20, 2, 0x2a, 4, 0x0a, 2, 0x08, 3],
            Schema::Prewrite
        )
        .is_ok());
        assert!(inspect(&[0x18, 1, 0x22, 4, 0x22, 2, 0x08, 3], Schema::Commit).is_ok());
        assert!(inspect(&[], Schema::Rollback).is_ok());
        assert!(inspect(&[0x18, 1, 0x18, 2], Schema::Commit).is_err());
    }
}
