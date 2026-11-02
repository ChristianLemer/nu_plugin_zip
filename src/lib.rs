pub mod archive;

use nu_plugin::{Plugin, PluginCommand};

pub struct ZipPlugin;

impl Plugin for ZipPlugin {
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").into()
    }

    fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
        vec![]
    }
}
