#![no_main]
use libfuzzer_sys::fuzz_target;

use shroudb_acl::{AclRequirement, AuthContext, Grant, Scope};

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }

    // Use first byte to select context type
    let is_platform = data[0] & 1 == 1;
    let num_grants = (data[1] % 5) as usize;
    let scope_byte = data[2];
    let requirement_byte = data[3];

    // Build grants from remaining bytes
    let mut grants = Vec::new();
    let mut offset = 4;
    for _ in 0..num_grants {
        if offset >= data.len() {
            break;
        }
        let ns_len = (data[offset] % 32) as usize;
        offset += 1;
        if offset + ns_len > data.len() {
            break;
        }
        let ns = String::from_utf8_lossy(&data[offset..offset + ns_len]).to_string();
        offset += ns_len;

        let scopes = match scope_byte % 4 {
            0 => vec![Scope::Read],
            1 => vec![Scope::Write],
            2 => vec![Scope::Read, Scope::Write],
            _ => vec![],
        };
        grants.push(Grant {
            namespace: ns,
            scopes,
        });
    }

    let ctx = if is_platform {
        AuthContext::platform("fuzz-tenant", "fuzzer")
    } else {
        AuthContext::tenant("fuzz-tenant", "fuzzer", grants, None)
    };

    // Build a requirement
    let requirement = match requirement_byte % 4 {
        0 => AclRequirement::None,
        1 => AclRequirement::Admin,
        2 => {
            let ns = if offset < data.len() {
                String::from_utf8_lossy(&data[offset..]).to_string()
            } else {
                "test".to_string()
            };
            AclRequirement::Namespace {
                ns,
                scope: if scope_byte & 2 == 0 {
                    Scope::Read
                } else {
                    Scope::Write
                },
                tenant_override: None,
            }
        }
        _ => {
            let ns = "cross-tenant".to_string();
            AclRequirement::Namespace {
                ns,
                scope: Scope::Read,
                tenant_override: Some("other-tenant".to_string()),
            }
        }
    };

    // Must never panic
    let _ = ctx.check(&requirement);
    let _ = ctx.accessible_namespaces();
    let _ = ctx.is_expired(data.get(0).copied().unwrap_or(0) as u64 * 1_000_000);
});
