use crate::{Error, Result};
use std::{fs::File, io::BufRead, path::Path};

pub fn read_fixture(path: impl AsRef<Path>) -> Result<Vec<crate::types::RawMessage>> {
    let file = File::open(path.as_ref()).map_err(Error::Io)?;
    let reader = std::io::BufReader::new(file);
    let mut messages = Vec::new();

    for (idx, line) in reader.lines().enumerate() {
        let line = line.map_err(Error::Io)?;
        if line.trim().is_empty() {
            continue;
        }
        let message = serde_json::from_str(&line).map_err(|source| Error::Fixture {
            line: idx + 1,
            source,
        })?;
        messages.push(message);
    }

    Ok(messages)
}
