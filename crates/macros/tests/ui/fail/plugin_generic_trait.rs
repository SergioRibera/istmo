#[istmo::plugin(name = "ui.generic_trait")]
pub trait Store<T> {
    async fn get(&self) -> u32;
}

fn main() {}
