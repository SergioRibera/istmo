#[istmo::plugin(name = "ui.missing_receiver")]
pub trait Free {
    async fn make(seed: u32) -> u32;
}

fn main() {}
