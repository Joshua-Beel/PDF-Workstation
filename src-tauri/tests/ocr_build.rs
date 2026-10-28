#[allow(dead_code)]
#[path = "../ocr_build.rs"]
mod ocr_build;

// The spawn lock lives in the app binary; nothing in this test binary creates inheritable child pipes on another thread, so a plain spawn cannot race.
#[allow(dead_code)]
mod child_job {
    pub(crate) fn test_output(command: &mut std::process::Command) -> std::io::Result<std::process::Output> {
        use std::process::Stdio;
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        command.spawn()?.wait_with_output()
    }
}
