#![cfg(target_os = "linux")]

use brewfs_csi::{MountOps, MountSpec, SystemMountOps, VolumeId};
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

// Keep PATH overrides in a subprocess so other tests never use our commands.
#[test]
fn timeout_reaps_child_and_cleans_late_mount() {
    let temp = tempfile::tempdir().unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    script(
        &bin.join("brewfs"),
        "#!/bin/sh\necho $$ > \"$CSI_TIMEOUT_TEST_ROOT/pid\"\nexec /bin/sleep 300\n",
    );
    script(
        &bin.join("mountpoint"),
        r#"#!/bin/sh
if [ -e "$CSI_TIMEOUT_TEST_ROOT/mounted" ]; then exit 0; fi
count=0
if [ -e "$CSI_TIMEOUT_TEST_ROOT/polls" ]; then
    read -r count < "$CSI_TIMEOUT_TEST_ROOT/polls"
fi
count=$((count + 1))
echo "$count" > "$CSI_TIMEOUT_TEST_ROOT/polls"
# Simulate a mount becoming visible just after the final negative observation.
if [ "$count" -eq 300 ]; then : > "$CSI_TIMEOUT_TEST_ROOT/mounted"; fi
exit 1
"#,
    );
    script(
        &bin.join("umount"),
        r#"#!/bin/sh
read -r pid < "$CSI_TIMEOUT_TEST_ROOT/pid"
if [ -e "/proc/$pid" ]; then
    echo 'child must be reaped before unmount' >&2
    exit 1
fi
rm "$CSI_TIMEOUT_TEST_ROOT/mounted"
: > "$CSI_TIMEOUT_TEST_ROOT/unmounted"
"#,
    );
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "timeout_subprocess", "--ignored", "--nocapture"])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env("CSI_TIMEOUT_TEST_ROOT", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "invoked in an isolated subprocess by timeout_reaps_child_and_cleans_late_mount"]
fn timeout_subprocess() {
    let root = std::path::PathBuf::from(std::env::var_os("CSI_TIMEOUT_TEST_ROOT").unwrap());
    let spec = MountSpec {
        volume: VolumeId::parse("brewfs://cluster-a/00000000-0000-0000-0000-000000000007").unwrap(),
        config: root.join("config.yaml"),
        binary: root.join("bin/brewfs"),
        mount_root: root.join("mounts"),
        read_only: false,
    };
    let error = SystemMountOps
        .start_brewfs(&spec, &spec.shared_mount())
        .unwrap_err();
    let pid = fs::read_to_string(root.join("pid")).unwrap();
    let reaped = !Path::new("/proc").join(pid.trim()).exists();
    let mounted = SystemMountOps.is_mounted(&spec.shared_mount()).unwrap();
    let unmounted = root.join("unmounted").exists();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        reaped && !mounted && unmounted,
        "reaped={reaped}, late_mount_present={mounted}, unmount_called={unmounted}"
    );
}
