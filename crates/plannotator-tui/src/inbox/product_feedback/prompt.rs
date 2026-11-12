use serde_json::Value;

use super::model::Draft;

pub(super) fn text(draft: &Draft, settings: Option<&Value>) -> String {
    let quoted = draft.message.trim().lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
    let started = if quoted.is_empty() {
        String::new()
    } else {
        format!(" Start from what I wrote so far:\n\n{quoted}\n")
    };
    let contact = if draft.contact.trim().is_empty() { "No contact provided" } else { draft.contact.trim() };
    let fallback = installed(settings).map_or_else(String::new, |command| {
        format!(" If `plannotator` is not found, use {command} instead.")
    });
    format!(
        r#"Help me send feedback about Plannotator to its maintainers.

1. Ask me what I want to say: what works, what doesn't, what I'd like. Keep my words; tidy them only if I ask.{started}
2. Show me the exact text you will send, with a category (bug, idea or other) and contact only if I give one. My current category: {category}. Current contact: {contact}. Add nothing from our files, code or conversation.
3. Only after I say yes, send it. The text goes on stdin exactly as written, so the shell changes nothing. Never put it in double quotes: they expand $VAR, backticks and $(...). Sending also includes the Plannotator version and OS name.

In bash, zsh or sh, a quoted heredoc:

```
plannotator feedback --stdin <<'PLANNOTATOR_FEEDBACK_END'
<the exact text I approved>
PLANNOTATOR_FEEDBACK_END
```

In PowerShell, a single-quoted here-string with UTF-8:

```
$OutputEncoding = [Text.UTF8Encoding]::new($false)
@'
<the exact text I approved>
'@ | plannotator feedback --stdin
```

Add --category bug|idea|other only for my choice. Add --contact only if I gave one, quoted as a single shell argument; escape a ' as '\'' in bash or '' in PowerShell. If a text line is exactly PLANNOTATOR_FEEDBACK_END, choose another delimiter. If a PowerShell text line starts with '@, use the bash form instead. The command prints "Sent". `plannotator feedback --help` has the details.{fallback}
4. Tell me it was sent, or what went wrong."#,
        category = draft.category.label()
    )
}

fn installed(settings: Option<&Value>) -> Option<String> {
    let command = settings?.get("mcp_command")?.as_array()?;
    let [binary, inbox, mcp] = command.as_slice() else { return None };
    if inbox.as_str() != Some("inbox") || mcp.as_str() != Some("mcp") {
        return None;
    }
    let binary = binary.as_str()?;
    if binary.chars().any(char::is_control) {
        return None;
    }
    let name = binary.rsplit(['/', '\\']).next()?;
    if !name.eq_ignore_ascii_case("plannotator") && !name.eq_ignore_ascii_case("plannotator.exe") {
        return None;
    }
    if binary == "plannotator" {
        return None;
    }
    Some(format!("the installed binary `{binary}` (quote its path for your shell)"))
}
