
pub mod app;
pub mod ui;
pub mod theme;
pub mod remote;
pub mod run;

use crate::url;

pub fn start_shell(remote: Option<url::RemoteDest>) -> anyhow::Result<()> {
    app::run(remote)
}
