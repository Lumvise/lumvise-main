use std::{env, path::Path};

fn main() {
    if let Err(error) = run(env::args().skip(1).collect()) {
        eprintln!("plugin administration failed: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: Vec<String>) -> Result<(), String> {
    let [command, release_dir] = arguments.as_slice() else {
        return Err("expected: lumvise-plugin-admin install-release <release-dir>".into());
    };
    if command != "install-release" {
        return Err(format!(
            "unknown command `{command}`; expected `install-release`"
        ));
    }
    let installed = lumvise_plugin_admin::install_release(Path::new(release_dir))
        .map_err(|error| error.to_string())?;
    for plugin in installed {
        println!("enabled {}@{}", plugin.plugin_id, plugin.version);
    }
    Ok(())
}
