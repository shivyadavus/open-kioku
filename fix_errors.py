import re

with open('crates/open-kioku-graph/src/query.rs', 'r') as f:
    content = f.read()

content = content.replace('OkError::Unsupported(', 'GraphQueryError::ParseError(')
content = content.replace('GraphQueryError::ParseError("Query execution timed out".into())', 'GraphQueryError::Timeout')
content = content.replace('GraphQueryError::ParseError(format!("Write-like or unsupported keyword rejected: {}", ident))', 'GraphQueryError::QueryRejected(format!("Write-like or unsupported keyword rejected: {}", ident))')
content = content.replace('GraphQueryError::ParseError(format!("Unknown node type: {}", s))', 'GraphQueryError::UnknownNodeType(s)')
content = content.replace('GraphQueryError::ParseError(format!("Unknown edge type: {}", s))', 'GraphQueryError::UnknownEdgeType(s)')
content = content.replace('GraphQueryError::ParseError(format!("Unsupported filter field: {}", field))', 'GraphQueryError::UnsupportedFilter(field)')

with open('crates/open-kioku-graph/src/query.rs', 'w') as f:
    f.write(content)
