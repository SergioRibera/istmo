#[istmo::plugin(name = "ui.default_body")]
pub trait Defaulted {
    async fn answer(&self) -> u32 {
        42
    }
}

fn main() {}
