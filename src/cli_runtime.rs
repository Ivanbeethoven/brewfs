/// Use Tokio's CPU-based worker selection and its environment override.
pub(crate) fn build() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests {
    #[test]
    fn worker_count_child() {
        let Ok(expected) = std::env::var("BREWFS_TEST_RUNTIME_WORKERS") else {
            return;
        };
        let runtime = super::build().unwrap();
        let expected_workers: usize = expected.parse().unwrap();
        assert_eq!(runtime.metrics().num_workers(), expected_workers);
        assert_eq!(
            runtime.block_on(async { tokio::spawn(async { 42 }).await.unwrap() }),
            42
        );
    }

    #[test]
    fn default_and_environment_override() {
        // Isolate environment changes from concurrently running Rust tests.
        for (setting, expected) in [(None, std::thread::available_parallelism().unwrap().get()), (Some("2"), 2)] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child
                .arg("worker_count_child")
                .env("BREWFS_TEST_RUNTIME_WORKERS", expected.to_string());
            if let Some(setting) = setting {
                child.env("TOKIO_WORKER_THREADS", setting);
            } else {
                child.env_remove("TOKIO_WORKER_THREADS");
            }
            let output = child.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
        }
    }
}
