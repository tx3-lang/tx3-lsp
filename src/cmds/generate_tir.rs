use serde_json::{json, Value};
use tx3_tir::reduce::Apply;
use crate::{Context, Error};

#[derive(Debug)]
pub struct Args {
    document_url: String,
    tx_name: String,
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
            tx_name: value
                .get(1)
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned())
                .ok_or(Error::InvalidCommandArgs("tx_name".to_string()))?,
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

    let tx = tx3_lang::lowering::lower(&program, &args.tx_name).unwrap();

    let tir = tx3_tir::encoding::to_bytes(&tx);

    let out = json!({
        "tir": hex::encode(&tir.0),
        "version": tir.1,
        "parameters": tx.params(),
    });

    Ok(Some(out))
}
