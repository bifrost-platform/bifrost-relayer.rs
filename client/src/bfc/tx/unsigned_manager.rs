use br_primitives::tx::XtRequestMessage;
use tokio::sync::mpsc::UnboundedSender;

/// The message sender connected to the tx request channel.
pub struct XtRequestSender<Call> {
	/// The inner sender.
	pub sender: UnboundedSender<XtRequestMessage<Call>>,
}

impl<Call> XtRequestSender<Call> {
	/// Instantiates a new `XtRequestSender` instance.
	pub fn new(sender: UnboundedSender<XtRequestMessage<Call>>) -> Self {
		Self { sender }
	}

	/// Sends a new tx request message.
	pub fn send(
		&self,
		message: XtRequestMessage<Call>,
	) -> Result<(), SendError<XtRequestMessage<Call>>> {
		self.sender.send(message)
	}
}
