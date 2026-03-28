# Multi-User Local Profiles Implementation Plan

**Date**: March 27, 2026  
**Status**: Design  
**Epic**: Multi-user local profiles with optional device PIN lock

---

## Overview

Implement Steam-style auto-login and multi-user support on a single machine. Each user has their own encrypted database file (`pollis_{userId}.db`). On startup, if a valid session exists, auto-login. If logged out, show known accounts and "login as different user" option. Email OTP remains the only auth method. Optional device PIN can be enabled from settings to require unlock on app reopen (PIN not stored, fallback to OTP if forgotten).

---

## Goals

- Auto-login for last active user (if session valid)
- Support multiple local accounts per machine with secure separation
- Single auth method (email OTP) — no additional password/PIN required
- Per-user encrypted database isolation
- Move window state to DB (debounced to 1s max)
- Optional PIN lock for device-level security
- Minimize OS keystore calls (avoid user prompts)
- No breaking changes to multi-device migration plan

---

## Architecture

### Storage Locations by OS

**Database Files & Account Index:**
- **Linux**: `~/.local/share/pollis/`
- **macOS**: `~/Library/Application Support/com.pollis.app/`
- **Windows**: `%APPDATA%/pollis/`

**Secure Keystore (for secrets):**
- **Debug builds**: `{DB_DIR}/dev-keystore.json` (JSON file, no OS keychain)
- **Release/Linux**: libsecret/pass (system keyring)
- **Release/macOS**: Keychain
- **Release/Windows**: Credential Manager

### Storage Layout

```
{DB_DIR}/
├── accounts.json                 # Account index + last_active_user
├── pollis_{user_id_1}.db        # User 1 encrypted DB
├── pollis_{user_id_2}.db        # User 2 encrypted DB
└── ...

OS Keystore (varies by platform):
├── pollis:db_key_{user_id_1}    # Per-user DB encryption key (SERVICE:KEY format)
├── pollis:db_key_{user_id_2}
├── pollis:session_{user_id_1}   # Per-user session token
├── pollis:session_{user_id_2}
├── pollis:identity_key_private  # Global (same across users on device)
└── pollis:identity_key_public   # Global
```

**Debug Mode Override:**
- Set `POLLIS_DATA_DIR` environment variable to use custom path
- Keystore entries are namespaced with directory name: `{dirname}:key_{user_id}`
- Prevents collisions when running multiple dev instances

### accounts.json Format

```json
{
  "accounts": [
    {
      "user_id": "user_abc123",
      "username": "alice",
      "avatar_url": "https://...",
      "last_seen": "2026-03-27T15:30:00Z"
    },
    {
      "user_id": "user_def456",
      "username": "bob",
      "avatar_url": "https://...",
      "last_seen": "2026-03-25T10:15:00Z"
    }
  ],
  "last_active_user": "user_abc123"
}
```

### Session Token Storage

**Decision**: Store in OS keystore (Option B)
- Store as `session_{user_id}` in keystore
- Minimizes DB dependency on auth state
- Keeps DB purely for app data

---

## Data Schema Changes

### New Local DB Tables

Add to `local_schema.sql`:

```sql
CREATE TABLE IF NOT EXISTS ui_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Insert initial record for window_state
INSERT OR IGNORE INTO ui_state (key, value) 
VALUES ('window_state', '{"width": 1024, "height": 768, "x": 0, "y": 0}');
```

### Keystore Keys

**New per-user keys:**
- `db_key_{user_id}` — 32-byte DB encryption key
- `session_{user_id}` — Auth token

**Existing keys (keep):**
- `identity_key_private` — Ed25519 signing key (still global for this device)
- `identity_key_public` — Ed25519 public key (still global)
- `local_db_key` — **DEPRECATED** (migrate to `db_key_{user_id}`)

**New global keys:**
- `active_user_id` — Current active user (optional, for quick access)

---

## File Changes (Detailed)

### 1. `src-tauri/src/db/local.rs`

**Current signature:**
```rust
impl LocalDb {
    pub fn open(key: &[u8]) -> Result<Self>
}
```

**Change to:**
```rust
impl LocalDb {
    pub fn open_for_user(user_id: &str, key: &[u8]) -> Result<Self> {
        // Path: dirs_path().join(format!("pollis_{}.db", user_id))
        // Rest of logic same (encryption, schema check, PRAGMA)
    }
    
    // Keep for backward compat (tests/migration):
    pub fn open(key: &[u8]) -> Result<Self> {
        // Deprecated: calls open_for_user("__default__", key)
    }
}

// New helper:
pub fn list_local_dbs() -> Result<Vec<String>> {
    // Scan data_dir for pollis_*.db files
    // Extract and return user IDs
}
```

**Platform-Specific Database Paths (via existing `dirs_path()`):**

- **Linux**: `~/.local/share/pollis/pollis_{user_id}.db`
  - Actual: `$HOME/.local/share/pollis/pollis_{user_id}.db`
  - Falls back to current dir if `$HOME` not set
- **macOS**: `~/Library/Application Support/com.pollis.app/pollis_{user_id}.db`
  - Actual: `$HOME/Library/Application Support/com.pollis.app/pollis_{user_id}.db`
  - Falls back to current dir if `$HOME` not set
- **Windows**: `%APPDATA%/pollis/pollis_{user_id}.db`
  - Actual: `{APPDATA env var}/pollis/pollis_{user_id}.db`
  - Falls back to current dir if `APPDATA` not set
- **Override (all platforms)**: If `POLLIS_DATA_DIR` env var is set, uses that dir instead

**Implementation Details:**
1. `dirs_path()` function already handles platform detection and env var override
2. Reuse `dirs_path()` directly — no need to duplicate logic
3. For per-user DB: `dirs_path().join(format!("pollis_{}.db", user_id))`
4. For accounts.json: `dirs_path().join("accounts.json")`
5. Ensure parent directories exist before opening DB (already done in current code)

---

### 2. `src-tauri/src/keystore.rs`

**Add helper for per-user keys:**
```rust
fn user_key(key: &str, user_id: &str) -> String {
    format!("{}_{}", key, user_id)
}

// Public functions:
pub async fn store_for_user(key: &str, user_id: &str, value: &[u8]) -> Result<()> {
    let namespaced = user_key(key, user_id);
    store(&namespaced, value).await
}

pub async fn load_for_user(key: &str, user_id: &str) -> Result<Option<Vec<u8>>> {
    let namespaced = user_key(key, user_id);
    load(&namespaced).await
}

pub async fn delete_for_user(key: &str, user_id: &str) -> Result<()> {
    let namespaced = user_key(key, user_id);
    delete(&namespaced).await
}
```

**Important Implementation Notes:**

1. **Existing `store()`, `load()`, `delete()` remain unchanged** — they delegate to platform-specific backends
2. **Debug mode** (`#[cfg(debug_assertions)]`):
   - Keys stored in `{DB_DIR}/dev-keystore.json`
   - Each key gets prefixed with `DEV:` in debug builds
   - Additional `POLLIS_DATA_DIR` namespacing applies
   - No OS keychain/credential manager calls — no prompts
3. **Release mode** (`#[cfg(not(debug_assertions))]`):
   - **Linux**: Uses `keyring` crate → libsecret/pass backend
   - **macOS**: Uses `keyring` crate → Keychain
   - **Windows**: Uses `keyring` crate → Credential Manager
   - SERVICE name is hardcoded as `"pollis"`
   - Each entry is `Entry::new("pollis", &namespaced_key)`
4. **Key format in OS keystore**:
   - Debug: `DEV:key_{user_id}` or `{dirname}:DEV:key_{user_id}`
   - Release: `key_{user_id}` (service="pollis")
5. **Error handling**:
   - Linux/macOS/Windows: `keyring::Error::NoEntry` returns `Ok(None)` (not found)
   - File I/O errors on Linux/macOS/Windows will return error (keyring not available)
   - Debug mode: JSON read errors return empty map (graceful degradation)

---

### 3. `src-tauri/src/state.rs`

**Update AppState:**
```rust
pub struct AppState {
    pub config: Config,
    pub current_user_id: Arc<Mutex<Option<String>>>,
    pub local_db: Arc<Mutex<Option<LocalDb>>>,  // None until user loaded
    pub remote_db: Arc<RemoteDb>,
    pub otp_store: Arc<Mutex<HashMap<String, OtpEntry>>>,
    pub livekit: Arc<Mutex<LiveKitState>>,
    pub update_required: Arc<AtomicBool>,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self> {
        // Don't open DB here; defer until user selected
        // Only initialize remote_db and other shared resources
        
        Ok(Self {
            config,
            current_user_id: Arc::new(Mutex::new(None)),
            local_db: Arc::new(Mutex::new(None)),
            remote_db: Arc::new(remote),
            otp_store: Arc::new(Mutex::new(HashMap::new())),
            livekit: Arc::new(Mutex::new(LiveKitState::new())),
            update_required: Arc::new(AtomicBool::new(false)),
        })
    }

    pub async fn load_user_db(&self, user_id: &str) -> Result<()> {
        let db_key = keystore::load_for_user("db_key", user_id).await?
            .ok_or(Error::Other(anyhow::anyhow!("no db key for user {}", user_id)))?;
        
        let db = LocalDb::open_for_user(user_id, &db_key)?;
        *self.local_db.lock().await = Some(db);
        *self.current_user_id.lock().await = Some(user_id.to_string());
        
        Ok(())
    }

    pub async fn unload_user_db(&self) {
        *self.local_db.lock().await = None;
        *self.current_user_id.lock().await = None;
    }
}
```

---

### 4. `src-tauri/src/commands/auth.rs`

**Update `verify_otp` command:**
```rust
#[tauri::command]
pub async fn verify_otp(
    email: String,
    code: String,
    state: tauri::State<'_, AppState>,
) -> Result<User> {
    // Existing email + code verification (unchanged)
    let user = verify_with_backend(&email, &code).await?;
    
    // NEW: Setup user DB
    let user_id = &user.id;
    
    // 1. Generate or load DB key from keystore
    let db_key = match keystore::load_for_user("db_key", user_id).await? {
        Some(k) => k,
        None => {
            let key: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
            keystore::store_for_user("db_key", user_id, &key).await?;
            key
        }
    };
    
    // 2. Open user's DB
    state.load_user_db(user_id).await?;
    
    // 3. Store session token in keystore
    keystore::store_for_user("session", user_id, user.session_token.as_bytes()).await?;
    
    // 4. Update accounts.json
    update_accounts_index(user_id, &user.username, &user.avatar_url).await?;
    
    // 5. Set as last active
    set_last_active_user(user_id).await?;
    
    Ok(user)
}
```

**New command `get_session`:**
```rust
#[tauri::command]
pub async fn get_session(state: tauri::State<'_, AppState>) -> Result<Option<User>> {
    // 1. Read accounts.json
    let accounts = read_accounts_index().await?;
    let last_active = accounts.last_active_user.as_ref();
    
    if let Some(user_id) = last_active {
        // 2. Try load session from keystore
        if let Some(token) = keystore::load_for_user("session", user_id).await? {
            // 3. Validate token with backend (quick call)
            if let Ok(user) = validate_session_token(user_id, &String::from_utf8(token)?).await {
                // 4. Load that user's DB
                state.load_user_db(user_id).await?;
                return Ok(Some(user));
            }
        }
    }
    
    Ok(None)
}
```

**New command `list_known_users`:**
```rust
#[tauri::command]
pub async fn list_known_users() -> Result<Vec<AccountInfo>> {
    let accounts = read_accounts_index().await?;
    Ok(accounts.accounts)
}
```

**New command `switch_user`:**
```rust
#[tauri::command]
pub async fn switch_user(user_id: String, state: tauri::State<'_, AppState>) -> Result<()> {
    state.unload_user_db().await;
    state.load_user_db(&user_id).await?;
    set_last_active_user(&user_id).await?;
    Ok(())
}
```

**Update `logout` command:**
```rust
#[tauri::command]
pub async fn logout(delete_data: bool, state: tauri::State<'_, AppState>) -> Result<()> {
    let user_id = state.current_user_id.lock().await.clone();
    
    if let Some(uid) = user_id {
        // Clear session token
        keystore::delete_for_user("session", &uid).await?;
        
        // Optional: delete data
        if delete_data {
            let db_path = /* pollis_{uid}.db path */;
            std::fs::remove_file(db_path)?;
            keystore::delete_for_user("db_key", &uid).await?;
            remove_from_accounts_index(&uid).await?;
        }
        
        // Clear active user
        state.unload_user_db().await;
        clear_last_active_user().await?;
    }
    
    Ok(())
}
```

---

### 5. `src-tauri/src/commands/user.rs`

**Update `get_preferences` and `set_preferences`:**

These already call `local_db` via `state.local_db` — handle `Option<LocalDb>` (None if user not loaded).

```rust
#[tauri::command]
pub async fn get_preferences(
    user_id: String,
    state: tauri::State<'_, AppState>,
) -> Result<String> {
    let local_db = state.local_db.lock().await;
    let db = local_db.as_ref()
        .ok_or(Error::Other(anyhow::anyhow!("no db loaded")))?;
    
    let prefs: Option<String> = db.conn().query_row(
        "SELECT preferences FROM user_preferences WHERE user_id = ?1",
        rusqlite::params![user_id],
        |row| row.get(0),
    ).ok();
    
    Ok(prefs.unwrap_or_default())
}
```

---

### 6. `src-tauri/src/commands/ui.rs` (NEW FILE)

**New command for window state:**
```rust
use tauri::State;
use crate::AppState;
use crate::error::Result;

#[tauri::command]
pub async fn get_ui_state(key: String, state: State<'_, AppState>) -> Result<Option<String>> {
    let local_db = state.local_db.lock().await;
    let db = local_db.as_ref()
        .ok_or(crate::error::Error::Other(anyhow::anyhow!("no db loaded")))?;
    
    let value: Option<String> = db.conn().query_row(
        "SELECT value FROM ui_state WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get(0),
    ).ok();
    
    Ok(value)
}

#[tauri::command]
pub async fn set_ui_state(key: String, value: String, state: State<'_, AppState>) -> Result<()> {
    let local_db = state.local_db.lock().await;
    let db = local_db.as_ref()
        .ok_or(crate::error::Error::Other(anyhow::anyhow!("no db loaded")))?;
    
    db.conn().execute(
        "INSERT OR REPLACE INTO ui_state (key, value, updated_at) VALUES (?1, ?2, datetime('now'))",
        rusqlite::params![key, value],
    )?;
    
    Ok(())
}
```

Register in `lib.rs`:
```rust
mod commands {
    pub mod auth;
    pub mod user;
    pub mod ui;  // NEW
    // ...
}

// In invoke_handler:
.invoke_handler(tauri::generate_handler![
    commands::auth::verify_otp,
    commands::auth::get_session,
    commands::auth::list_known_users,
    commands::auth::switch_user,
    commands::auth::logout,
    commands::ui::get_ui_state,  // NEW
    commands::ui::set_ui_state,  // NEW
    // ...
])
```

---

### 7. `src-tauri/src/accounts.rs` (NEW FILE)

```rust
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct AccountInfo {
    pub user_id: String,
    pub username: String,
    pub avatar_url: String,
    pub last_seen: String,
}

#[derive(Serialize, Deserialize)]
pub struct AccountsIndex {
    pub accounts: Vec<AccountInfo>,
    pub last_active_user: Option<String>,
}

fn accounts_file() -> PathBuf {
    crate::db::local::dirs_path().join("accounts.json")
}

pub async fn read_accounts() -> crate::error::Result<AccountsIndex> {
    let path = accounts_file();
    if !path.exists() {
        return Ok(AccountsIndex {
            accounts: vec![],
            last_active_user: None,
        });
    }
    let json = tokio::fs::read_to_string(&path).await?;
    Ok(serde_json::from_str(&json)?)
}

pub async fn write_accounts(index: &AccountsIndex) -> crate::error::Result<()> {
    let path = accounts_file();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let json = serde_json::to_string_pretty(index)?;
    tokio::fs::write(&path, json).await?;
    Ok(())
}

pub async fn add_or_update_account(
    user_id: String,
    username: String,
    avatar_url: String,
) -> crate::error::Result<()> {
    let mut index = read_accounts().await?;
    
    index.accounts.retain(|a| a.user_id != user_id);
    index.accounts.push(AccountInfo {
        user_id,
        username,
        avatar_url,
        last_seen: chrono::Utc::now().to_rfc3339(),
    });
    
    write_accounts(&index).await?;
    Ok(())
}

pub async fn remove_account(user_id: &str) -> crate::error::Result<()> {
    let mut index = read_accounts().await?;
    index.accounts.retain(|a| a.user_id != user_id);
    if index.last_active_user.as_deref() == Some(user_id) {
        index.last_active_user = None;
    }
    write_accounts(&index).await?;
    Ok(())
}

pub async fn set_last_active(user_id: &str) -> crate::error::Result<()> {
    let mut index = read_accounts().await?;
    index.last_active_user = Some(user_id.to_string());
    write_accounts(&index).await?;
    Ok(())
}

pub async fn clear_last_active() -> crate::error::Result<()> {
    let mut index = read_accounts().await?;
    index.last_active_user = None;
    write_accounts(&index).await?;
    Ok(())
}
```

Add to `src-tauri/src/lib.rs`:
```rust
mod accounts;
```

---

### 8. `frontend/src/App.tsx`

**Update startup flow:**
```typescript
async function checkStoredSession() {
  try {
    // Check for required update before anything else (skip in dev)
    if (!import.meta.env.DEV) {
      const { check: checkUpdate } = await import("@tauri-apps/plugin-updater");
      const update = await checkUpdate();
      if (update) {
        await invoke("mark_update_required");
        setAppState("update-required");
        return;
      }
    }

    // Check for active session + load user DB
    const user = await api.getSession();
    if (user) {
      try {
        await api.initializeIdentity(user.id);
      } catch (err) {
        console.error("[App] Failed to initialize identity:", err);
      }
      
      // Load preferences from local DB
      try {
        const json = await invoke<string>("get_preferences", { userId: user.id });
        const prefs = {
          accent_color: getPreference<string | undefined>(json, "accent_color", undefined),
          background_color: getPreference<string | undefined>(json, "background_color", undefined),
          font_size: getPreference<string | undefined>(json, "font_size", undefined),
        };
        applyPreferences(prefs);
      } catch {
        // Preferences are optional
      }
      
      setCurrentUser(user);
      setAppState("ready");
    } else {
      setAppState("email-auth");
    }
  } catch (error) {
    console.error("[App] Error checking session:", error);
    setAppState("email-auth");
  }
}
```

**Add new UI state:**
```typescript
type AppState = 
  | "initializing" 
  | "loading" 
  | "email-auth" 
  | "logout-confirm" 
  | "identity-setup" 
  | "update-required" 
  | "ready";
```

---

### 9. `frontend/src/hooks/useWindowState.ts`

**Update to save to DB instead of localStorage:**
```typescript
export async function restoreWindowState(): Promise<void> {
  try {
    const appWindow = getCurrentWindow();
    
    // Try to load from DB
    const rawJson = await invoke<string | null>("get_ui_state", { key: "window_state" });
    const raw = rawJson ? rawJson : null;
    
    if (!raw) {
      await appWindow.center();
      return;
    }
    
    const parsed: unknown = JSON.parse(raw);
    if (!isValidWindowState(parsed)) {
      await appWindow.center();
      return;
    }

    const onScreen = await isPositionOnScreen(parsed.x, parsed.y);
    await appWindow.setSize(new LogicalSize(parsed.width, parsed.height));

    if (onScreen) {
      await appWindow.setPosition(new LogicalPosition(parsed.x, parsed.y));
    } else {
      await appWindow.center();
    }
  } catch {
    // Best-effort — ignore failures
  }
}

export function useWindowState(): void {
  useEffect(() => {
    const appWindow = getCurrentWindow();
    let saveTimeout: ReturnType<typeof setTimeout>;

    const save = async () => {
      try {
        const [size, position, scale] = await Promise.all([
          appWindow.innerSize(),
          appWindow.outerPosition(),
          appWindow.scaleFactor(),
        ]);
        const state: WindowState = {
          width: Math.round(size.width / scale),
          height: Math.round(size.height / scale),
          x: Math.round(position.x / scale),
          y: Math.round(position.y / scale),
        };
        
        // Save to DB instead of localStorage
        await invoke("set_ui_state", { 
          key: "window_state", 
          value: JSON.stringify(state) 
        });
      } catch {
        // Ignore failures (DB may not be loaded yet)
      }
    };

    const schedulesSave = () => {
      clearTimeout(saveTimeout);
      saveTimeout = setTimeout(save, 1000);  // 1s debounce
    };

    let unlistenResize: (() => void) | undefined;
    let unlistenMove: (() => void) | undefined;

    const setup = async () => {
      unlistenResize = await appWindow.onResized(schedulesSave);
      unlistenMove = await appWindow.onMoved(schedulesSave);
    };

    setup();

    return () => {
      clearTimeout(saveTimeout);
      unlistenResize?.();
      unlistenMove?.();
    };
  }, []);
}
```

---

## Platform-Specific Implementation Notes

### Linux (Keyring: libsecret/pass)

**File Locations:**
```
~/.local/share/pollis/
├── accounts.json
└── pollis_*.db files
```

**Keystore Backend:**
- Uses `keyring::Entry` → libsecret (GNOME) or pass (fallback)
- SERVICE: `"pollis"`
- Key format: `key_{user_id}`, `session_{user_id}`, etc.
- **No user prompts** — silent background access (libsecret integrates with systemd/user session)

**Potential Issues:**
- If libsecret daemon not running, keyring calls fail silently
- In headless/SSH environments, may need manual keyring unlock
- Debug mode avoids this: uses `dev-keystore.json` instead

**Testing:**
```bash
# Check available keys
secret-tool search service pollis

# Verify file permissions
ls -la ~/.local/share/pollis/
```

---

### macOS (Keychain)

**File Locations:**
```
~/Library/Application Support/com.pollis.app/
├── accounts.json
└── pollis_*.db files
```

**Keystore Backend:**
- Uses `keyring::Entry` → macOS Keychain
- SERVICE: `"pollis"`
- Key format: `key_{user_id}`, `session_{user_id}`, etc.
- **May show prompt** on first access (user approves app accessing keychain)
- Subsequent accesses typically silent (if "Always Allow" selected)

**Potential Issues:**
- First keystore access triggers Keychain dialog
- If user denies, keystore access fails
- Keychain locked (login required): operations fail silently

**Testing:**
```bash
# View Keychain entries
security find-generic-password -s pollis

# Verify file permissions
ls -la ~/Library/Application\ Support/com.pollis.app/
```

---

### Windows (Credential Manager)

**File Locations:**
```
%APPDATA%/pollis/
├── accounts.json
└── pollis_*.db files
```
(Typically: `C:\Users\{username}\AppData\Roaming\pollis\`)

**Keystore Backend:**
- Uses `keyring::Entry` → Windows Credential Manager (CredentialUI)
- SERVICE: `"pollis"`
- Key format: `key_{user_id}`, `session_{user_id}`, etc.
- **No user prompts** — accesses Credential Manager silently (no UAC)

**Potential Issues:**
- Credential Manager may lock on logout/session change
- Network credentials different from local credentials
- BitLocker/full-disk encryption may affect access

**Testing:**
```powershell
# View stored credentials
cmdkey /list:* | findstr pollis

# Verify folder permissions
ls "C:\Users\$env:USERNAME\AppData\Roaming\pollis"
```

---

### Debug Mode (All Platforms)

**File Locations:**
```
{POLLIS_DATA_DIR}/  (if set, else same as release above)
├── dev-keystore.json
├── accounts.json
└── pollis_*.db files
```

**Keystore Backend:**
- Plain JSON file: `dev-keystore.json`
- No OS keychain calls
- **No prompts ever** — ideal for CI/testing/headless
- Keys prefixed with `DEV:` (and optional `{dirname}:` if POLLIS_DATA_DIR set)

**Example:**
```json
{
  "DEV:db_key_user123": "base64-encoded-key",
  "DEV:session_user123": "base64-encoded-token"
}
```

**Security Note:** Dev keystore is **NOT encrypted**. For development only.

---

## Error Handling Strategy

### Keystore Access Failures

**If `load_for_user("db_key", user_id)` fails:**
1. Log the error
2. If user attempting to login → guide to re-authenticate (re-run `verify_otp`)
3. If user attempting to auto-login → show login screen (session unavailable)

**If `store_for_user()` fails:**
1. Log the error
2. User-facing: "Unable to save session, please login again"
3. Don't crash — graceful fallback

**If `delete_for_user()` fails on logout:**
1. Log the error
2. Proceed anyway (session cleared from memory)
3. Next app start will fail to find token → user re-logs in

### Database File Access Failures

**If `open_for_user()` fails:**
1. Check if file exists but corrupted → delete and recreate
2. Check permissions — must be writable
3. Check disk space
4. If all checks fail → show error modal, don't auto-start

---

## Migration from Single-User to Multi-User

### Detection Logic

```rust
// Check if old single-user DB exists
let old_path = dirs_path().join("pollis.db");
let new_path = dirs_path().join(format!("pollis_{}.db", user_id));

if old_path.exists() && !new_path.exists() {
    // Migrate
    std::fs::rename(&old_path, &new_path)?;
    // Update accounts.json
    add_or_update_account(user_id, username, avatar_url).await?;
}
```

### Keystore Migration

```rust
// Check if old single-user key exists
if let Ok(Some(old_key)) = keystore::load("local_db_key").await {
    // Store under new scheme
    keystore::store_for_user("db_key", &user_id, &old_key).await?;
    // Delete old key
    keystore::delete("local_db_key").await.ok();
}
```

---

## Migration from Single-User to Multi-User (Complete)

**Current single-user installs:**
- First app launch after update:
  - Detect existing `pollis.db` (no user ID in name)
  - Detect existing `local_db_key` in keystore (old scheme) — uses `keystore::load("local_db_key")`
  - Prompt user OR auto-migrate in dev
  - Rename DB file: `pollis.db` → `pollis_{user_id}.db`
  - Migrate keystore key: `local_db_key` → `db_key_{user_id}` (using `keystore::store_for_user()`)
  - Delete old keystore entry: `keystore::delete("local_db_key")`
  - Create `accounts.json` index with `last_active_user` set
  - Call `add_or_update_account(user_id, username, avatar_url)` to populate accounts list

**No data loss** — all existing DB content preserved.

**Keystore Behavior During Migration:**
- Debug builds: Read from/write to `dev-keystore.json` (no OS calls)
- Release builds: May prompt on first key store/load on macOS (user approves Keychain access)
- Must handle platform-specific errors gracefully (keyring daemon down, etc.)

---

## Testing Checklist

### Core Functionality (All Platforms)
- [ ] Open app, auto-login last active user
- [ ] Logout, show login screen
- [ ] Create new account, verify new DB file created
- [ ] Verify accounts.json updated correctly with both users
- [ ] Switch users, verify data isolation (no bleed between DBs)
- [ ] Window state: resize/move, restart app, verify position restored
- [ ] Logout + delete data, verify DB file + keystore entries removed
- [ ] Multi-user scenarios: 3+ accounts, rapid switching
- [ ] Migration from single-user to multi-user DB
- [ ] Session expiry: clear token, verify login screen shows

### Platform-Specific Testing

#### Linux
- [ ] Debug mode: verify `dev-keystore.json` created in correct dir
- [ ] Release mode: verify keys stored in libsecret (check via `secret-tool`)
- [ ] Verify DB files created in `~/.local/share/pollis/`
- [ ] Test with headless environment (no display)
- [ ] Verify no keyring prompts appear

#### macOS
- [ ] Debug mode: verify `dev-keystore.json` in `~/Library/Application Support/com.pollis.app/`
- [ ] Release mode: verify keys in Keychain (check Keychain Access.app)
- [ ] **First keystore access may prompt** — verify prompt appears and works
- [ ] Verify DB files created in `~/Library/Application Support/com.pollis.app/`
- [ ] Test Keychain locked scenario (lock/unlock system)

#### Windows
- [ ] Debug mode: verify `dev-keystore.json` in `%APPDATA%\pollis\`
- [ ] Release mode: verify keys in Credential Manager (run `cmdkey /list`)
- [ ] Verify DB files created in `%APPDATA%\pollis\`
- [ ] Test with different user accounts
- [ ] Verify no UAC prompts appear

### File System Checks
- [ ] accounts.json format is valid JSON
- [ ] accounts.json has both users listed
- [ ] accounts.json `last_active_user` correct
- [ ] DB file permissions correct (readable/writable by user)
- [ ] DB files encrypted (check with hexdump — should not see plaintext)
- [ ] Old `pollis.db` file removed after migration

### Keystore Checks
- [ ] **Debug**: `dev-keystore.json` entries have format `DEV:db_key_{id}` or `{dirname}:DEV:db_key_{id}`
- [ ] **Linux Release**: `secret-tool search service pollis` shows entries
- [ ] **macOS Release**: Keychain Access shows pollis entries
- [ ] **Windows Release**: `cmdkey /list | findstr pollis` shows entries
- [ ] Old `local_db_key` entry deleted after migration
- [ ] Session tokens not persisted after logout

### Error Scenarios
- [ ] Delete keystore entry manually, attempt login → graceful error + re-login works
- [ ] Delete DB file manually, app attempts to open → recreates with schema
- [ ] Corrupt accounts.json → app handles gracefully, creates new one
- [ ] No disk space → proper error message (not crash)
- [ ] Permissions denied on DB file → error message (not crash)

---

## Rollout Plan

### Phase 1 (Core multi-user)
1. Implement `LocalDb::open_for_user()`, keystore user keys, accounts index
2. Update auth flow: `verify_otp` → `get_session`, `list_known_users`
3. Implement `switch_user`, updated `logout`
4. Frontend: app startup flow
5. Window state move to DB (debounce 1s)
6. Testing + bugfixes

### Phase 2 (Optional PIN, deferred)
1. Add PIN setting + keystore storage
2. Startup PIN prompt + verification
3. PIN recovery via OTP login
4. Testing + doc update

---

## Backward Compatibility

- Existing single-user DB migrations handled automatically
- Multi-device migration plan (`device_id`) unaffected
- All Tauri commands check for loaded DB before use
- React Query cache unchanged (still in-memory)

---

## Security Considerations

- Each DB encrypted with unique key in OS keystore ✓
- Session tokens stored in keystore (not DB) ✓
- PIN not stored, only derived on unlock (Phase 2) ✓
- Cross-user data isolation via separate files ✓
- No user prompts (OS keystore calls minimal and silent) ✓
- Fallback to email OTP if session expires/forgotten PIN ✓

---

## File Manifest

### New Files
- `src-tauri/src/accounts.rs` — Account index management
- `src-tauri/src/commands/ui.rs` — UI state (window geometry)

### Modified Files
- `src-tauri/src/db/local.rs` — Add `open_for_user()`, list functions
- `src-tauri/src/keystore.rs` — Add `*_for_user()` helpers
- `src-tauri/src/state.rs` — Add `current_user_id`, lazy DB loading
- `src-tauri/src/commands/auth.rs` — Refactor auth commands
- `src-tauri/src/commands/user.rs` — Ensure DB-aware preference fetch
- `src-tauri/src/lib.rs` — Register new commands + modules
- `src-tauri/src/db/migrations/local_schema.sql` — Add `ui_state` table
- `frontend/src/App.tsx` — Update startup flow
- `frontend/src/hooks/useWindowState.ts` — DB-backed window state

### Unchanged
- Remote DB + Turso queries
- Signal protocol logic
- Multi-device migration
- React Query patterns
