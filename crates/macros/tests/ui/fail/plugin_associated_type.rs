#[istmo::plugin(name = "ui.associated_type")]
pub trait WithType {
    type Item;

    async fn get(&self) -> u32;
}

fn main() {}
