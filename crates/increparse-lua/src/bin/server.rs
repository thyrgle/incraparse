//! `increparse-lua-server`: run a Lua-defined language server.
//!
//! ```text
//! increparse-lua-server <path/to/language.lua>
//! ```

use std::error::Error;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: increparse-lua-server <language.lua>");
        std::process::exit(2);
    };

    let language = increparse_lua::LuaLanguage::from_path(&path)?;
    eprintln!(
        "increparse-lua-server: serving language {:?} from {path}",
        language.name()
    );

    increparse_lsp::serve(language)
}
