
use futures::{stream, Stream};
use std::{pin::Pin, sync::Arc};
use tokio_stream::wrappers::ReceiverStream;

use crate::{CommandResponse, Publish, Subscribe, Unsubscribe};
use crate::service::topic::Topic;

pub type StreamingResponse = Pin<Box<dyn Stream<Item = Arc<CommandResponse>> + Send>>;

pub trait TopicService {
    /// 处理 Command，返回 Response
    fn execute(self, topic: impl Topic) -> StreamingResponse;
}

impl TopicService for Subscribe {
    fn execute(self, topic: impl Topic) -> StreamingResponse {
        let rx = topic.subscribe(self.topic);
        Box::pin(ReceiverStream::new(rx))
    }
}

impl TopicService for Unsubscribe {
    fn execute(self, topic: impl Topic) -> StreamingResponse {
        let res = match topic.unsubscribe(self.topic, self.id) {
            Ok(_) => CommandResponse::ok(),
            Err(e) => e.into(),
        };
        Box::pin(stream::once(async { Arc::new(res) }))
    }
}

impl TopicService for Publish {
    fn execute(self, topic: impl Topic) -> StreamingResponse {
        topic.publish(self.topic, Arc::new(self.data.into()));
        Box::pin(stream::once(async { Arc::new(CommandResponse::ok()) }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::{convert::TryInto, time::Duration};
    use futures::StreamExt;
    use crate::{assert_res_ok, assert_res_error, Broadcaster, CommandRequest, dispatch_stream, Topic};
    use tokio::time;
    use tracing::debug;

    #[tokio::test]
    async fn dispatch_publish_should_work() {
        let topic = Arc::new(Broadcaster::default());
        let cmd = CommandRequest::new_publish("lobby", vec!["hello".into()]);
        let mut res = dispatch_stream(cmd, topic);
        let data = res.next().await.unwrap();
        assert_res_ok(&data, &[], &[]);
    }


    #[tokio::test]
    async fn dispatch_subscribe_should_work() {
        let topic = Arc::new(Broadcaster::default());
        let cmd = CommandRequest::new_subscribe("lobby");
        let mut res = dispatch_stream(cmd, topic);
        let id: i64 = res.next().await.unwrap().as_ref().try_into().unwrap();
        assert!(id > 0);
    }


    #[tokio::main]
    async fn dispatch_subscribe_abnormal_quit_should_work() {
        let topic = Arc::new(Broadcaster::default());
        let id = {
            let cmd = CommandRequest::new_subscribe("lobby");
            let mut res = dispatch_stream(cmd, topic.clone());
            let id: i64 = res.next().await.unwrap().as_ref().try_into().unwrap();
            drop(res);
            id as u32
        };

        // publish 时， 这个 subscription 已经失效，所有会被删除
        let cmd = CommandRequest::new_publish("lobby", vec!["hello".into()]);
        dispatch_stream(cmd, topic.clone());
        time::sleep(Duration::from_millis(10)).await;

        // 如果在此尝试删除，应该返回 KvError
        let result = topic.unsubscribe("lobby".into(), id);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn dispatch_unsubscribe_should_work() {
        let topic = Arc::new(Broadcaster::default());
        let cmd = CommandRequest::new_subscribe("lobby");
        let mut res = dispatch_stream(cmd, topic.clone());
        let id: i64 = res.next().await.unwrap().as_ref().try_into().unwrap();

        let cmd = CommandRequest::new_unsubscribe("lobby", id as _);
        let mut res = dispatch_stream(cmd, topic);
        let data = res.next().await.unwrap();

        assert_res_ok(&data, &[], &[]);
    }

    #[tokio::test]
    async fn dispatch_unsubscribe_random_id_should_error() {
        let topic = Arc::new(Broadcaster::default());
        let cmd = CommandRequest::new_unsubscribe("lobby", 9527);
        let mut res = dispatch_stream(cmd, topic);
        let data = res.next().await.unwrap();
        debug!("{:?}", data);
        assert_res_error(&data, 404, "Not found: subscription 9527");
    }

}