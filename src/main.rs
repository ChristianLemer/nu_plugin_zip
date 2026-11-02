use nu_plugin::{serve_plugin, MsgPackSerializer};
use nu_plugin_zip::ZipPlugin;

fn main() {
    serve_plugin(&ZipPlugin::new(), MsgPackSerializer);
}
