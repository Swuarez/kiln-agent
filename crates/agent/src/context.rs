//! Context budgeting.
//!
//! Three escalating moves: shrink stale tool output, drop the oldest whole
//! turns, then hard-truncate whatever is still biggest. Tool-call/tool-result
//! pairs stay intact throughout, because a split pair is a hard API error
//! rather than a soft failure.

use kiln_core::Message;

const COMPACTED_MARKER: &str = "[earlier output omitted]";
/// Never compact or drop the newest messages: they are the current turn.
const KEEP_RECENT: usize = 6;
const MIN_CHUNK: usize = 200;

pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages.iter().map(|m| m.approx_tokens()).sum()
}

pub fn fit(messages: &[Message], budget_tokens: usize) -> Vec<Message> {
    let mut out = messages.to_vec();
    if estimate_tokens(&out) <= budget_tokens {
        return out;
    }

    let floor = out.len().saturating_sub(KEEP_RECENT);
    for i in 0..floor {
        if let Message::Tool {
            tool_call_id,
            content,
            is_error,
        } = &out[i]
        {
            if content.len() > COMPACTED_MARKER.len() * 2 {
                out[i] = Message::tool(
                    tool_call_id.clone(),
                    format!("{COMPACTED_MARKER} ({} bytes)", content.len()),
                    *is_error,
                );
            }
        }
        if estimate_tokens(&out) <= budget_tokens {
            return out;
        }
    }

    while estimate_tokens(&out) > budget_tokens && out.len() > KEEP_RECENT {
        let count = group_len(&out);
        out.drain(0..count);
        // A tool result without its call is invalid, so never leave one first.
        while matches!(out.first(), Some(Message::Tool { .. })) {
            out.remove(0);
        }
    }

    shrink_to_budget(&mut out, budget_tokens);
    out
}

/// Length of the atomic unit at the front: an assistant turn plus the tool
/// results that answer it.
fn group_len(messages: &[Message]) -> usize {
    match messages.first() {
        Some(Message::Assistant { tool_calls, .. }) if !tool_calls.is_empty() => {
            1 + messages[1..].iter().take_while(|m| m.is_tool()).count()
        }
        _ => 1,
    }
}

/// Last resort: halve the largest text body until the budget holds. Only
/// `content` is touched, never tool arguments, so calls stay well-formed.
fn shrink_to_budget(messages: &mut [Message], budget_tokens: usize) {
    for _ in 0..512 {
        if estimate_tokens(messages) <= budget_tokens {
            return;
        }
        let mut target: Option<(usize, usize)> = None;
        for (i, m) in messages.iter().enumerate() {
            let len = m.text().len();
            if len > MIN_CHUNK && target.map_or(true, |(_, best)| len > best) {
                target = Some((i, len));
            }
        }
        let Some((index, len)) = target else { return };
        messages[index] = shrink_message(&messages[index], len / 2);
    }
}

fn shrink_message(msg: &Message, keep: usize) -> Message {
    let text = msg.text();
    let mut cut = keep.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let shortened = format!(
        "{}\n[… {} bytes truncated to fit the context budget …]",
        &text[..cut],
        text.len() - cut
    );
    match msg.clone() {
        Message::System { .. } => Message::system(shortened),
        Message::User { .. } => Message::user(shortened),
        Message::Assistant { tool_calls, .. } => Message::assistant_calls(shortened, tool_calls),
        Message::Tool {
            tool_call_id,
            is_error,
            ..
        } => Message::tool(tool_call_id, shortened, is_error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kiln_core::ToolCall;

    fn assistant_with_tools(id: &str, n: usize) -> Message {
        Message::assistant_calls(
            "",
            (0..n)
                .map(|i| ToolCall {
                    id: format!("{id}_{i}"),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "x"}),
                })
                .collect(),
        )
    }

    #[test]
    fn small_conversations_are_untouched() {
        let messages = vec![Message::user("hi"), Message::assistant("hello")];
        assert_eq!(fit(&messages, 10_000), messages);
    }

    #[test]
    fn stale_tool_output_is_compacted() {
        let mut messages = vec![Message::user("task")];
        for i in 0..10 {
            messages.push(assistant_with_tools(&format!("c{i}"), 1));
            messages.push(Message::tool(format!("c{i}_0"), "x".repeat(40_000), false));
        }
        let out = fit(&messages, 4_000);
        assert!(estimate_tokens(&out) <= 4_000, "{}", estimate_tokens(&out));
        assert!(out
            .iter()
            .filter(|m| m.is_tool())
            .any(|m| m.text().contains("omitted") || m.text().contains("truncated")));
    }

    #[test]
    fn tool_pairs_are_never_split() {
        let mut messages = vec![Message::user("task")];
        for i in 0..20 {
            messages.push(assistant_with_tools(&format!("c{i}"), 2));
            messages.push(Message::tool(format!("c{i}_0"), "y".repeat(2_000), false));
            messages.push(Message::tool(format!("c{i}_1"), "y".repeat(2_000), false));
        }
        let out = fit(&messages, 1_500);
        assert!(!matches!(out.first(), Some(Message::Tool { .. })));
        for (i, msg) in out.iter().enumerate() {
            if let Message::Assistant { tool_calls, .. } = msg {
                let answers = out[i + 1..].iter().take_while(|m| m.is_tool()).count();
                assert!(
                    answers >= tool_calls.len(),
                    "assistant at {i} has {} calls but {answers} results",
                    tool_calls.len()
                );
            }
        }
        assert!(estimate_tokens(&out) <= 1_500, "{}", estimate_tokens(&out));
    }

    #[test]
    fn budget_holds_for_a_long_flat_conversation() {
        let mut messages = Vec::new();
        for i in 0..200 {
            messages.push(Message::user(format!("message {i} {}", "z".repeat(100))));
        }
        let out = fit(&messages, 500);
        assert!(estimate_tokens(&out) <= 500, "{}", estimate_tokens(&out));
        assert!(!out.is_empty());
    }

    #[test]
    fn truncation_is_utf8_safe() {
        let messages = vec![Message::user("é".repeat(4_000))];
        let out = fit(&messages, 50);
        assert!(out[0].text().contains("truncated"));
    }
}
