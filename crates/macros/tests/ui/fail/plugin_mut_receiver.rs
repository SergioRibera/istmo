#[istmo::plugin(name = "ui.mut_receiver")]
pub trait Counter {
    async fn bump(&mut self) -> u32;
}

fn main() {}
