//! Bounded stdio transport using the official SDK codec and session lifecycle.
use crate::{gateway::Gateway, server::Server};
use futures::StreamExt;
use rmcp::{
    service::{RoleServer, RxJsonRpcMessage, TxJsonRpcMessage},
    transport::{async_rw::JsonRpcMessageCodec, stdio},
    ServiceExt,
};
use std::sync::Arc;
use tokio_util::codec::{FramedRead, FramedWrite};

pub async fn serve(gateway: Arc<Gateway>) -> Result<(), String> {
    let (read, write) = stdio();
    let input = FramedRead::new(
        read,
        JsonRpcMessageCodec::<RxJsonRpcMessage<RoleServer>>::new_with_max_length(crate::MAX_FRAME),
    )
    .filter_map(|message| futures::future::ready(message.ok()));
    let output = FramedWrite::new(
        write,
        JsonRpcMessageCodec::<TxJsonRpcMessage<RoleServer>>::new(),
    );
    let transport = (output, input);
    let service = Server(gateway.clone())
        .serve(transport)
        .await
        .map_err(|e| e.to_string())?;
    let result = service.waiting().await.map_err(|e| e.to_string());
    gateway.drain().await;
    result.map(|_| ())
}
