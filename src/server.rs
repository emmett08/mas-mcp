//! MCP tool definitions and protocol-to-gateway argument conversion.
use crate::{gateway::Gateway, Result, MAX_FRAME, MAX_RESULT};
use rmcp::{
    model::*,
    service::{RequestContext, RoleServer},
    ServerHandler,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

#[derive(Clone)]
pub struct Server(pub Arc<Gateway>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submit {
    request_id: String,
    operation: String,
    input_json: String,
    descriptor_digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Task {
    task_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observe {
    task_id: String,
    #[serde(default)]
    include_payloads: bool,
}

fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|_| "arguments do not satisfy the tool schema".into())
}

impl Server {
    async fn dispatch(&self, name: &str, arguments: Value) -> Result<Value> {
        if arguments.to_string().len() > MAX_FRAME {
            return Err("tool arguments exceed limit".into());
        }
        match name {
            "mas_describe" => {
                if arguments != json!({}) {
                    return Err("mas_describe takes no arguments".into());
                }
                Ok(self.0.describe())
            }
            "mas_submit" => {
                let args: Submit = decode(arguments)?;
                self.0.submit(
                    &args.request_id,
                    &args.operation,
                    &args.input_json,
                    &args.descriptor_digest,
                )
            }
            "mas_status" => self.0.status(&decode::<Task>(arguments)?.task_id),
            "mas_observe" => {
                let args: Observe = decode(arguments)?;
                self.0.observe(&args.task_id, args.include_payloads).await
            }
            "mas_cancel" => self.0.cancel(&decode::<Task>(arguments)?.task_id).await,
            _ => Err("unknown tool".into()),
        }
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mas-mcp",env!("CARGO_PKG_VERSION")))
            .with_instructions("Use mas_describe, then mas_submit with its descriptor_digest and exact JSON encoded as input_json. Poll mas_status. Repeating an identity observes the retained task. An unknown outcome must be reconciled by the operator before any replacement request. Cancellation records intent only. Application outputs and observations are JSON strings to preserve exact values.")
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResponse, ErrorData> {
        let result = match self
            .dispatch(
                &request.name,
                Value::Object(request.arguments.unwrap_or_default()),
            )
            .await
        {
            Ok(value) => CallToolResult::structured(value),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        let result = if serde_json::to_vec(&result).map_or(true, |v| v.len() > MAX_RESULT - 4096) {
            CallToolResult::error(vec![ContentBlock::text(
                "response exceeds transport limit; retained task identity remains valid",
            )])
        } else {
            result
        };
        Ok(result.into())
    }
}

fn tools() -> Vec<Tool> {
    let text = || json!({"type":"string"});
    let task = json!({"task_id":text()});
    vec![
        definition("mas_describe","Describe the explicitly published MAS operations and their schemas.",json!({}),&[],true,false),
        definition("mas_submit","Durably submit a published operation. The operation may have external effects. Retrying the same identity observes its retained task without dispatching again.",
            json!({"request_id":text(),"operation":text(),"input_json":{"type":"string","maxLength":32768,"description":"Exact-integer JSON object matching the published input schema; decimals use application-defined strings or scaled integers."},"descriptor_digest":text()}),
            &["request_id","operation","input_json","descriptor_digest"],false,true),
        definition("mas_status","Read a retained task. completed means execution completed; inspect output_json for the application decision. unknown forbids automatic retry.",task.clone(),&["task_id"],true,false),
        definition("mas_observe","Read committed Nehemiah messages and host effects. Payloads require explicit server permission and caller opt-in.",
            json!({"task_id":text(),"include_payloads":{"type":"boolean","default":false}}),&["task_id"],true,false),
        definition("mas_cancel","Record cancellation intent. Only cancellation before dispatch is confirmed; external work can complete afterwards.",task,&["task_id"],false,true),
    ]
}

fn definition(
    name: &'static str,
    description: &'static str,
    properties: Value,
    required: &[&str],
    read: bool,
    open: bool,
) -> Tool {
    Tool::new(name,description,Arc::new(json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}).as_object().unwrap().clone()))
        .with_annotations(ToolAnnotations::new().read_only(read).destructive(!read).idempotent(true).open_world(open))
}
