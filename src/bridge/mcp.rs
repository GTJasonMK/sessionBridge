use super::*;
use serde_json::json;
use std::io::BufReader;

const OPERATIONS: &[&str] = &[
    "skill.read",
    "inspect",
    "ready",
    "history",
    "room.get",
    "message.send",
    "message.ack",
    "work.create",
    "work.update",
    "check.run",
    "room.integrate",
];

pub fn serve(context: &Path) -> Result<()> {
    let context: Context = serde_json::from_slice(&std::fs::read(context)?)?;
    let mut input = BufReader::new(std::io::stdin());
    let mut output = std::io::stdout();
    while let Some(message) = ipc::read_frame(&mut input)? {
        let Some(id) = message.get("id") else {
            continue;
        };
        let result = (|| -> Result<Value> {
            match message["method"].as_str().unwrap_or("") {
                "initialize" => Ok(
                    json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"sessionbridge","version":"2"}}),
                ),
                "ping" => Ok(json!({})),
                "tools/list" => Ok(
                    json!({"tools":[{"name":"sessionbridge","description":"Send addressed group messages, acknowledge processed requests and update assigned work. The authenticated system context supplies the group ID. Reuse a UUID request_id to retry the same database mutation.","inputSchema":{"type":"object","properties":{"operation":{"type":"string","enum":OPERATIONS},"args":{"type":"object","additionalProperties":true}},"required":["operation","args"],"additionalProperties":false}}]}),
                ),
                "tools/call" => {
                    let params = &message["params"];
                    if params["name"] != "sessionbridge" {
                        Err(error("UNKNOWN_TOOL", "unknown tool"))
                    } else {
                        let args = &params["arguments"];
                        let operation = text(args, "operation")?;
                        if !OPERATIONS.contains(&operation) {
                            Err(error("FORBIDDEN", "operation unavailable to agents"))
                        } else {
                            let call = ipc::call(&Request {
                                workspace: context.workspace.clone(),
                                operation: operation.into(),
                                args: args["args"].clone(),
                                token: context.token.clone(),
                            });
                            Ok(match call {
                                Ok(value) => {
                                    json!({"content":[{"type":"text","text":serde_json::to_string(&value)?}]})
                                }
                                Err(e) => {
                                    json!({"isError":true,"content":[{"type":"text","text":serde_json::to_string(&e)?}]})
                                }
                            })
                        }
                    }
                }
                _ => Err(error("METHOD_NOT_FOUND", "unsupported MCP method")),
            }
        })();
        let response = match result {
            Ok(v) => json!({"jsonrpc":"2.0","id":id,"result":v}),
            Err(e) => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":e.to_string()}})
            }
        };
        ipc::write_frame(&mut output, &response)?;
    }
    Ok(())
}
