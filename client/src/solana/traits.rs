#[async_trait::async_trait]
pub trait Handler {
	async fn run(&mut self);

	async fn process_event(&self, event_tx: Event);

	fn is_target_event(&self, event_type: EventType) -> bool;
}
