//! `incraparse-lua-server`: run a Lua-defined language server.
//!
//! ```text
//! incraparse-lua-server <path/to/language.lua>
//! ```

use std::error::Error;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: incraparse-lua-server <language.lua>");
        std::process::exit(2);
    };

    let language = incraparse_lua::LuaLanguage::from_path(&path)?;
    eprintln!(
        "incraparse-lua-server: serving language {:?} from {path}",
        language.name()
    );

    incraparse_lsp::serve(language)
}
