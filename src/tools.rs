use crate::tool::bash::BashTool;
use crate::tool::read::ReadTool;
use crate::tool::Tool;

/// 基础工具池（对标 getAllBaseTools 的最小面）。
pub fn base_tools() -> Vec<Box<dyn Tool>> {
    vec![Box::new(BashTool::new()), Box::new(ReadTool::new())]
}
