//! Preserve MCP images on the provider wire while keeping binary data out of
//! transcript text. Attachments use the same private, digest-bound store as
//! user images, so a handoff can reuse a screenshot without fetching it again.
use crate::{Error, Result, attachments};
use base64::Engine;
use serde_json::{Value, json};
use std::path::Path;
use xcb_core::{MAX_JSON_BYTES, MAX_TEXT_BYTES, session::Attachment};

pub(crate) struct ToolOutput {
    pub reply: Value,
    pub text: String,
    pub attachments: Vec<Attachment>,
}

pub(crate) fn reopen(
    store: &crate::store::Store,
    session: &xcb_core::Id,
    arguments: &Value,
) -> Result<Value> {
    let id = arguments
        .get("id")
        .and_then(Value::as_str)
        .ok_or(Error::Protocol("stored image id"))?;
    if arguments.as_object().is_none_or(|map| map.len() != 1) {
        return Err(Error::Protocol("stored image arguments"));
    }
    let attachment = store
        .messages(session, 512)?
        .into_iter()
        .flat_map(|message| message.attachments)
        .find(|attachment| attachment.digest.as_str() == id)
        .ok_or(Error::Unavailable(
            "image is not in this session's recent history",
        ))?;
    let bytes = attachments::read(store.root(), &attachment)?;
    Ok(
        json!({"content":[{"type":"image", "mimeType":attachment.media_type,
        "data":base64::engine::general_purpose::STANDARD.encode(bytes)}]}),
    )
}

fn rejected(message: impl Into<String>) -> ToolOutput {
    let text = message.into();
    ToolOutput {
        reply: json!({"content":[{"type":"text","text":text}],"isError":true}),
        text,
        attachments: vec![],
    }
}

pub(crate) fn prepare(root: &Path, result: Result<Value>, mcp: bool) -> ToolOutput {
    let result = match result {
        Ok(result) => result,
        Err(error) => return rejected(error.to_string()),
    };
    if mcp {
        return image_result(root, result).unwrap_or_else(|error| rejected(error.to_string()));
    }
    let text = match serde_json::to_string(&result) {
        Ok(text) if text.len() <= MAX_TEXT_BYTES => text,
        _ => return rejected("tool result exceeds 256 KiB; request a smaller result"),
    };
    ToolOutput {
        reply: json!({"content":[{"type":"text","text":text}],"isError":false}),
        text,
        attachments: vec![],
    }
}

fn image_result(root: &Path, result: Value) -> Result<ToolOutput> {
    // Keep headroom for the provider-specific envelope in its 1 MiB frame.
    if serde_json::to_vec(&result)?.len() > MAX_JSON_BYTES - 64 * 1024 {
        return Err(Error::Unavailable(
            "tool image/result is too large; request a smaller screenshot or result",
        ));
    }
    let content = result["content"]
        .as_array()
        .filter(|items| items.len() <= 32)
        .ok_or(Error::Protocol("tool result content"))?;
    let failed = match result.get("isError") {
        None => false,
        Some(Value::Bool(failed)) => *failed,
        _ => return Err(Error::Protocol("tool result error flag")),
    };
    let mut wire = Vec::new();
    let mut text = String::new();
    let mut stored = Vec::new();
    for item in content {
        match item["type"].as_str() {
            Some("text") => {
                let value = item["text"]
                    .as_str()
                    .ok_or(Error::Protocol("tool result text"))?;
                text.push_str(value);
                text.push('\n');
                wire.push(json!({"type":"text","text":value}));
            }
            Some("image") if stored.len() < 4 => {
                let data = item["data"]
                    .as_str()
                    .ok_or(Error::Protocol("tool image data"))?;
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| Error::Protocol("tool image encoding"))?;
                let attachment = attachments::store(root, &bytes)?;
                if item["mimeType"].as_str() != Some(attachment.media_type.as_str()) {
                    return Err(Error::Protocol("tool image media type"));
                }
                text.push_str(&format!(
                    "[Screenshot {}: {}×{} {}]\n",
                    stored.len() + 1,
                    attachment.width,
                    attachment.height,
                    attachment.media_type
                ));
                wire.push(json!({"type":"image","data":data,"mimeType":attachment.media_type}));
                stored.push(attachment);
            }
            _ => {
                return Err(Error::Protocol(
                    "unsupported tool content; expected text or up to four images",
                ));
            }
        }
        if text.len() > MAX_TEXT_BYTES - 512 {
            return Err(Error::Unavailable(
                "tool result exceeds 256 KiB; request a smaller result",
            ));
        }
    }
    Ok(ToolOutput {
        reply: json!({"content":wire,"isError":failed}),
        text,
        attachments: stored,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screenshots_remain_images_and_binary_stays_out_of_transcript() {
        let directory = tempfile::tempdir().unwrap();
        let root = xcb_core::canonical(directory.path()).unwrap().join("state");
        let store = crate::store::Store::open(&root).unwrap();
        let image = attachments::from_rgba(store.root(), 2, 2, vec![255; 16]).unwrap();
        let data = base64::engine::general_purpose::STANDARD
            .encode(attachments::read(store.root(), &image).unwrap());
        let result = json!({"content":[{"type":"text","text":"Page ready"},{"type":"image","data":data,"mimeType":"image/png"}]});
        let prepared = prepare(store.root(), Ok(result), true);
        assert_eq!(prepared.reply["isError"], false);
        assert_eq!(prepared.reply["content"][1]["type"], "image");
        assert_eq!(prepared.attachments, vec![image]);
        assert!(prepared.text.contains("Page ready"));
        assert!(!prepared.text.contains(&data));
    }
    #[test]
    fn malformed_and_oversized_results_are_tool_errors() {
        let root = tempfile::tempdir().unwrap();
        for result in [
            json!({"content":[{"type":"image","data":"invalid","mimeType":"image/png"}]}),
            json!({"content":[{"type":"resource","resource":{"uri":"file:///private"}}]}),
            json!({"content":[{"type":"text","text":"x".repeat(MAX_JSON_BYTES)}]}),
        ] {
            assert_eq!(
                prepare(root.path(), Ok(result), true).reply["isError"],
                true
            );
        }
    }
}
