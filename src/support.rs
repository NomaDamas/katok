use anyhow::Result;

pub(crate) fn dependency_status(binary: &str) -> &'static str {
    match std::process::Command::new(binary).arg("--help").output() {
        Ok(_) => "present",
        Err(_) => "missing",
    }
}

pub(crate) fn print_payload<T>(json: bool, payload: &T) -> Result<()>
where
    T: serde::Serialize,
{
    if json {
        println!("{}", serde_json::to_string_pretty(payload)?);
    } else {
        println!("{}", serde_json::to_string(payload)?);
    }
    Ok(())
}
