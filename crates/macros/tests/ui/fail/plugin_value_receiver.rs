#[istmo::plugin(name = "ui.value_receiver")]
pub trait Consume {
    async fn take(self) -> u32;
}

fn main() {}
