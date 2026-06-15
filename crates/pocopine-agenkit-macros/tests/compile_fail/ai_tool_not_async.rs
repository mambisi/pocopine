#[pocopine_agenkit_macros::ai_tool]
fn lookup(input: u8, ctx: u8) -> Result<u8, u8> {
    Ok(input + ctx)
}

fn main() {}
