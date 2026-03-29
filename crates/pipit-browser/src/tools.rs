//! Browser tool definitions for the pipit tool registry.
//!
//! These tools are registered into ToolRegistry and available to the agent.

/// Tool names for browser integration.
pub const BROWSER_NAVIGATE: &str = "browser_navigate";
pub const BROWSER_SCREENSHOT: &str = "browser_screenshot";
pub const BROWSER_CONSOLE: &str = "browser_console";
pub const BROWSER_NETWORK: &str = "browser_network";
pub const BROWSER_CLICK: &str = "browser_click";
pub const BROWSER_TYPE: &str = "browser_type";
pub const BROWSER_A11Y: &str = "browser_a11y";
pub const BROWSER_LIGHTHOUSE: &str = "browser_lighthouse";

/// Tool schemas for browser tools.
pub fn browser_tool_schemas() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "name": BROWSER_NAVIGATE,
            "description": "Navigate headless browser to a URL",
            "input_schema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "URL to navigate to"}
                },
                "required": ["url"]
            }
        }),
        serde_json::json!({
            "name": BROWSER_SCREENSHOT,
            "description": "Take a screenshot of the current page or a specific element",
            "input_schema": {
                "type": "object",
                "properties": {
                    "selector": {"type": "string", "description": "Optional CSS selector for element screenshot"}
                }
            }
        }),
        serde_json::json!({
            "name": BROWSER_CONSOLE,
            "description": "Get all console messages (log/warn/error) since last call",
            "input_schema": {"type": "object", "properties": {}}
        }),
        serde_json::json!({
            "name": BROWSER_NETWORK,
            "description": "List failed network requests (4xx/5xx)",
            "input_schema": {"type": "object", "properties": {}}
        }),
        serde_json::json!({
            "name": BROWSER_CLICK,
            "description": "Click an element by CSS selector",
            "input_schema": {
                "type": "object",
                "properties": {
                    "selector": {"type": "string", "description": "CSS selector of element to click"}
                },
                "required": ["selector"]
            }
        }),
        serde_json::json!({
            "name": BROWSER_TYPE,
            "description": "Type text into a form field",
            "input_schema": {
                "type": "object",
                "properties": {
                    "selector": {"type": "string", "description": "CSS selector of input field"},
                    "text": {"type": "string", "description": "Text to type"}
                },
                "required": ["selector", "text"]
            }
        }),
    ]
}
