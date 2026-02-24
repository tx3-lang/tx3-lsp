use crate::{ast_to_svg::tx_to_svg, Context, Error};
use serde_json::{json, Value};

pub struct Args {
    document_url: String,
}

impl TryFrom<Vec<Value>> for Args {
    type Error = Error;

    fn try_from(value: Vec<Value>) -> Result<Self, Self::Error> {
        Ok(Args {
            document_url: value
                .get(0)
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .ok_or(Error::InvalidCommandArgs("document_url".to_string()))?,
        })
    }
}

pub async fn run(
    context: &Context,
    args: impl TryInto<Args, Error = Error>,
) -> Result<Option<Value>, Error> {
    let args: Args = args.try_into()?;

    let mut program = context.get_document_program(&args.document_url)?;

    tx3_lang::analyzing::analyze(&mut program).ok().unwrap();

    let tx_svgs: Vec<Value> = program
        .txs
        .iter()
        .map(|tx| {
            let svg = tx_to_svg(&program, tx);
            json!({
                "tx_name": tx.name.value,
                "svg": svg
            })
        })
        .collect();

    Ok(Some(Value::Array(tx_svgs)))
}
