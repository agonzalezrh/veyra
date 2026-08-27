import re
with open('build.rs', 'r') as f:
    content = f.read()
# Replace .file("test_xxx.c") with .file(format!("{}/test_xxx.c", manifest_dir))
content = content.replace(
    '.file("test_gbm_bo_get_fd_for_plane.c")',
    '.file(format!("{}/test_gbm_bo_get_fd_for_plane.c", manifest_dir))'
)
content = content.replace(
    '.file("test_gbm_bo_create_with_modifiers2.c")',
    '.file(format!("{}/test_gbm_bo_create_with_modifiers2.c", manifest_dir))'
)
# Add manifest_dir variable at start of each function
content = content.replace(
    'fn test_gbm_bo_fd_for_plane() {',
    'fn test_gbm_bo_fd_for_plane() {\n    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());'
)
content = content.replace(
    'fn test_gbm_bo_create_with_modifiers2() {',
    'fn test_gbm_bo_create_with_modifiers2() {\n    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());'
)
with open('build.rs', 'w') as f:
    f.write(content)
print("patched")
