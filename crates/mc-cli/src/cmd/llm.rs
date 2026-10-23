//! `eli5 llm models`: the model names the configured provider accepts, for a person to pick one.

use serde_json::json;

use crate::cli::{CliError, Ctx, Io, emit};

pub fn run(ctx: &Ctx<'_>, io: &mut Io<'_>) -> Result<u8, CliError> {
    let ["models"] = ctx
        .args
        .positional
        .as_slice()
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()[..]
    else {
        return Err(CliError::usage(
            "usage: eli5 llm models [--llm-provider P] [--base-url U]",
        ));
    };
    let mut config = ctx.cfg.checked_llm()?.clone();
    // The window asks about a server before its address is saved.
    if let Some(url) = ctx.args.value("base-url") {
        config.base_url = Some(url.to_string());
    }
    let models = mc_llm::list_models(&config).map_err(|e| CliError::Runtime {
        code: "llm",
        message: e.to_string(),
        hint: Some("`eli5 doctor` checks the llm settings".to_string()),
    })?;
    let mut human: String = models.iter().map(|m| format!("{m}\n")).collect();
    if human.is_empty() {
        human.push_str("this provider does not list its models; write the name it expects\n");
    }
    emit(
        io,
        ctx.json,
        human,
        json!({"provider": config.provider, "models": models}),
    )?;
    Ok(0)
}
