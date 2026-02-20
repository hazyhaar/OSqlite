/// Tool definitions for the OSqlite agentic loop.
///
/// These are sent in the `tools` array of the Anthropic Messages API request.
/// Claude uses them to read/write files, execute SQL, and list the namespace.

use alloc::string::String;
use alloc::format;

/// A tool definition with name, description, and JSON Schema for input.
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON Schema for the input object (compact JSON, no outer braces needed).
    pub input_schema: &'static str,
}

/// Tools exposed to Claude in the agentic loop.
pub const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "read_file",
        description: "Read a file from the OSqlite namespace. Returns the file content as a string, or an error if not found.",
        input_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Namespace path to read (e.g. /agents/indexer)"}},"required":["path"]}"#,
    },
    ToolDef {
        name: "write_file",
        description: "Write content to a file in the OSqlite namespace. Creates or overwrites the file.",
        input_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Namespace path to write"},"content":{"type":"string","description":"File content to write"}},"required":["path","content"]}"#,
    },
    ToolDef {
        name: "sql_query",
        description: "Execute a read-only SQL query on the OSqlite system database. Only SELECT, EXPLAIN, and PRAGMA are allowed.",
        input_schema: r#"{"type":"object","properties":{"query":{"type":"string","description":"SQL query to execute"}},"required":["query"]}"#,
    },
    ToolDef {
        name: "list_dir",
        description: "List entries in a namespace directory. Returns paths that start with the given prefix.",
        input_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Directory path to list (e.g. /agents/)"}},"required":["path"]}"#,
    },
    ToolDef {
        name: "str_replace",
        description: "Replace a specific string in a file. Reads the file, replaces the first occurrence of old_str with new_str, and writes back. Fails if old_str is not found.",
        input_schema: r#"{"type":"object","properties":{"path":{"type":"string","description":"Namespace path of the file to edit"},"old_str":{"type":"string","description":"Exact string to find and replace"},"new_str":{"type":"string","description":"Replacement string"}},"required":["path","old_str","new_str"]}"#,
    },
];

/// Serialize the tools array as JSON for the API request body.
pub fn tools_json() -> String {
    use super::escape_json;

    let mut out = String::from("[");
    for (i, tool) in TOOLS.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!(
            r#"{{"name":"{}","description":"{}","input_schema":{}}}"#,
            escape_json(tool.name),
            escape_json(tool.description),
            tool.input_schema,
        ));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_json_valid() {
        let json = tools_json();
        // Should parse as valid JSON array
        let parsed = super::super::json::parse(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 5);

        // Check first tool
        assert_eq!(arr[0].get("name").unwrap().as_str(), Some("read_file"));
        assert!(arr[0].get("input_schema").is_some());
    }

    #[test]
    fn test_all_schemas_valid_json() {
        for tool in TOOLS {
            let result = super::super::json::parse(tool.input_schema);
            assert!(result.is_ok(), "Invalid schema for tool '{}': {:?}", tool.name, result.err());
        }
    }
}
