//! The CLI's browser build is a compile-only compatibility target.
use std::path::Path;

use anyhow::{Result, bail};

use super::Prepared;

pub(super) fn shell_payload(_: &Prepared) -> Result<serde_json::Value> {
    bail!("locale HTML generation requires a host build")
}

pub fn prepare(_: &Path, _: bool) -> Result<Option<Prepared>> {
    bail!("locale build tools require a host target")
}
pub fn load(_: &Path) -> Result<Option<Prepared>> {
    bail!("locale build tools require a host target")
}
pub fn publish(_: &Path, _: &Prepared) -> Result<()> {
    bail!("locale build tools require a host target")
}
