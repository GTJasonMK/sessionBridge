use super::*;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::json;

const MEMBER_COLUMNS: &str =
    "id,room_id,name,role,cli,provider,model,state,generation,session,cwd,error";
pub struct Store {
    pub db: Connection,
    pub root: PathBuf,
    pub directory: PathBuf,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_in(root, &state_dir()?)
    }
    pub fn open_in(root: &Path, base: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        if !root.is_dir() {
            return Err(error("WORKSPACE", "workspace must be a directory"));
        }
        let name = root
            .to_str()
            .ok_or_else(|| error("INVALID_INPUT", "workspace path must be UTF-8"))?;
        let directory = base.join("workspaces").join(hash(name));
        crate::private_files::ensure_private_dir(&directory)?;
        let db = Connection::open(directory.join("state.db"))?;
        db.busy_timeout(std::time::Duration::from_secs(5))?;
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 2 {
            return Err(error(
                "SCHEMA_VERSION",
                "database is newer than this application",
            ));
        }
        if version < 2 {
            db.pragma_update(None, "foreign_keys", false)?;
            let transaction = Transaction::new_unchecked(&db, TransactionBehavior::Immediate)?;
            if version == 1 {
                transaction.execute_batch(include_str!("migrate_v2.sql"))?;
            }
            transaction.execute_batch(include_str!("schema.sql"))?;
            if version == 1 {
                transaction.execute(
                    "INSERT INTO message_changes(message_id) SELECT m.id FROM messages m WHERE NOT EXISTS (SELECT 1 FROM message_changes c WHERE c.message_id=m.id) ORDER BY m.seq",
                    [],
                )?;
            }
            let broken: bool = transaction
                .prepare("PRAGMA foreign_key_check")?
                .exists([])?;
            if broken {
                return Err(error("DATABASE", "migration violates foreign keys"));
            }
            transaction.commit()?;
            db.pragma_update(None, "foreign_keys", true)?;
        }
        if !directory.join("workspace.json").exists() {
            private_write(
                &directory.join("workspace.json"),
                &serde_json::to_vec(&json!({"root":root}))?,
            )?;
        }
        Ok(Self {
            db,
            root,
            directory,
        })
    }
    pub fn transaction<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let transaction = Transaction::new_unchecked(&self.db, TransactionBehavior::Immediate)?;
        let result = operation()?;
        transaction.commit()?;
        Ok(result)
    }
    pub fn rows(&self, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Value>> {
        let mut stmt = self.db.prepare(sql)?;
        let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let result = stmt
            .query_map(args, |row| {
                let mut object = serde_json::Map::new();
                for (i, name) in names.iter().enumerate() {
                    let value = match row.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        rusqlite::types::ValueRef::Real(n) => json!(n),
                        rusqlite::types::ValueRef::Text(_) => json!(row.get::<_, String>(i)?),
                        rusqlite::types::ValueRef::Blob(_) => {
                            return Err(rusqlite::Error::InvalidColumnType(
                                i,
                                name.clone(),
                                rusqlite::types::Type::Blob,
                            ));
                        }
                    };
                    object.insert(name.clone(), value);
                }
                Ok(Value::Object(object))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        result
            .into_iter()
            .map(|mut row| {
                for field in ["recipients", "dependencies", "details", "argv"] {
                    if let Some(value) = row.get(field) {
                        let parsed: Value =
                            serde_json::from_str(value.as_str().ok_or_else(|| {
                                error("DATABASE", format!("invalid {field} storage type"))
                            })?)?;
                        if field == "details" {
                            if !parsed.is_object() {
                                return Err(error("DATABASE", "invalid details object"));
                            }
                        } else if !parsed
                            .as_array()
                            .is_some_and(|v| v.iter().all(Value::is_string))
                        {
                            return Err(error("DATABASE", format!("invalid {field} array")));
                        }
                        row[field] = parsed;
                    }
                }
                Ok(row)
            })
            .collect()
    }
    pub fn member(&self, member: &str) -> Result<Value> {
        self.rows(
            &format!("SELECT {MEMBER_COLUMNS} FROM members WHERE id=?"),
            &[&member],
        )?
        .pop()
        .ok_or_else(|| error("NOT_FOUND", "member not found"))
    }
    pub fn room(&self, room: &str) -> Result<Value> {
        self.rows("SELECT * FROM rooms WHERE id=?", &[&room])?
            .pop()
            .ok_or_else(|| error("NOT_FOUND", "room not found"))
    }
    pub fn authenticate(&self, token: &str) -> Result<String> {
        self.db.query_row("SELECT id FROM members WHERE token_hash=? AND state IN ('starting','ready','busy')",[hash(token)],|r|r.get(0))
            .optional()?.ok_or_else(||error("UNAUTHORIZED","invalid or revoked member credential"))
    }
    pub fn authorize(&self, token: &str, operator: &str, operation: &str) -> Result<String> {
        if token == operator {
            return Ok("boss".into());
        }
        let actor = self.authenticate(token)?;
        if self.member(&actor)?["state"] == "starting"
            && ![
                "inspect",
                "skill.read",
                "history",
                "room.get",
                "ready",
                "message.ack",
            ]
            .contains(&operation)
        {
            return Err(error(
                "NOT_READY",
                "complete the handshake before participating",
            ));
        }
        Ok(actor)
    }
    fn room_access(&self, actor: &str, room: &str) -> Result<()> {
        self.room(room)?;
        if actor != "boss" && self.member(actor)?["room_id"] != room {
            return Err(error("FORBIDDEN", "cross-room access denied"));
        }
        Ok(())
    }
    fn coordinator(&self, actor: &str, room: &str) -> Result<()> {
        self.room_access(actor, room)?;
        if actor != "boss" && self.member(actor)?["role"] != "coordinator" {
            return Err(error("FORBIDDEN", "coordinator required"));
        }
        Ok(())
    }
    fn active(&self, room: &str) -> Result<()> {
        if self.room(room)?["state"] != "active" {
            return Err(error("ROOM_INACTIVE", "room is not active"));
        }
        Ok(())
    }
    pub fn system(&self, room: &str, body: &str) -> Result<Value> {
        self.insert_message(room, "system", "system", body, &[], None, 0)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn insert_message(
        &self,
        room: &str,
        sender: &str,
        kind: &str,
        body: &str,
        recipients: &[String],
        reply: Option<&str>,
        depth: i64,
    ) -> Result<Value> {
        let sender = if sender == "boss" {
            self.db.query_row(
                "SELECT id FROM members WHERE room_id=? AND role='boss'",
                [room],
                |r| r.get::<_, String>(0),
            )?
        } else {
            sender.to_owned()
        };
        if sender != "system" && self.member(&sender)?["room_id"] != room {
            return Err(error("FORBIDDEN", "message sender belongs to another room"));
        }
        let mid = id();
        self.db.execute("INSERT INTO messages(id,room_id,sender,kind,body,recipients,reply_to,depth,created) VALUES(?,?,?,?,?,?,?,?,?)",
            params![mid,room,sender,kind,body,serde_json::to_string(recipients)?,reply,depth,now()])?;
        let sequence = self.db.last_insert_rowid();
        for recipient in recipients {
            self.db.execute(
                "INSERT INTO deliveries(message_id,member_id,state) VALUES(?1,?2,CASE WHEN (SELECT role FROM members WHERE id=?2)='boss' THEN 'delivered' ELSE 'queued' END)",
                params![mid, recipient],
            )?;
        }
        Ok(json!({"id":mid,"seq":sequence}))
    }
    pub fn notify_coordinator(&self, room: &str, body: &str) -> Result<()> {
        let members = self.rows(
            "SELECT id FROM members WHERE room_id=? AND role='coordinator' AND state!='removed'",
            &[&room],
        )?;
        let recipients: Vec<String> = members
            .iter()
            .map(|m| text(m, "id").map(str::to_owned))
            .collect::<Result<_>>()?;
        self.insert_message(room, "system", "notice", body, &recipients, None, 0)?;
        Ok(())
    }
    fn transition(&self, room: &str, action: &str) -> Result<Value> {
        if !["active", "paused", "cancelled", "archived"].contains(&action) {
            return Err(error("INVALID_INPUT", "invalid room transition"));
        }
        let old = self.room(room)?;
        if old["state"] == "archived" || (old["state"] == "cancelled" && action != "archived") {
            return Err(error("ROOM_INACTIVE", "terminal room cannot resume"));
        }
        if action == "archived" && !matches!(old["state"].as_str(), Some("cancelled" | "completed"))
        {
            return Err(error("ROOM_ACTIVE", "complete or cancel before archiving"));
        }
        if action == "active" && old["state"] == "completed" {
            return Err(error("ROOM_INACTIVE", "create a new room for a new task"));
        }
        self.db
            .execute("UPDATE rooms SET state=? WHERE id=?", params![action, room])?;
        if action == "paused" || action == "cancelled" || action == "archived" {
            self.db.execute("UPDATE members SET token_hash=NULL,state=CASE WHEN state='removed' THEN state ELSE 'offline' END WHERE room_id=? AND role!='boss'",[room])?;
            self.db.execute("UPDATE deliveries SET state='queued' WHERE message_id IN (SELECT id FROM messages WHERE room_id=?) AND state='delivered'",[room])?;
            self.db.execute("UPDATE approvals SET decision='deny' WHERE member_id IN (SELECT id FROM members WHERE room_id=?) AND decision IS NULL",[room])?;
        }
        if action == "cancelled" {
            self.db.execute("UPDATE work SET state='cancelled',version=version+1 WHERE room_id=? AND state NOT IN ('done','cancelled')",[room])?;
        }
        self.system(room, &format!("工作群状态：{action}"))?;
        self.room(room)
    }
    pub fn execute(&self, actor: &str, operation: &str, args: &Value) -> Result<Value> {
        let read = matches!(
            operation,
            "workspace.get" | "room.list" | "room.get" | "history"
        );
        if read {
            return self.handle(actor, operation, args);
        }
        self.idempotent(actor, operation, args, || {
            self.handle(actor, operation, args)
        })
    }
    pub fn idempotent<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        actor: &str,
        operation: &str,
        args: &Value,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let request = text(args, "request_id")?;
        let fingerprint = hash(&format!("{operation}:{args}"));
        self.apply_idempotent(actor, request, fingerprint, action)
    }
    pub fn apply_idempotent<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        actor: &str,
        request: &str,
        fingerprint: String,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        uuid::Uuid::parse_str(request)
            .map_err(|_| error("INVALID_INPUT", "request_id must be a UUID"))?;
        self.transaction(|| {
            let previous: Option<(String, String)> = self
                .db
                .query_row(
                    "SELECT fingerprint,result FROM mutations WHERE actor=? AND request_id=?",
                    params![actor, request],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((old, result)) = previous {
                if old != fingerprint {
                    return Err(error(
                        "REQUEST_CONFLICT",
                        "request_id already used with different input",
                    ));
                }
                return Ok(serde_json::from_str(&result)?);
            }
            let value = action()?;
            self.db.execute(
                "INSERT INTO mutations VALUES(?,?,?,?)",
                params![actor, request, fingerprint, serde_json::to_string(&value)?],
            )?;
            Ok(value)
        })
    }
    fn handle(&self, actor: &str, operation: &str, args: &Value) -> Result<Value> {
        let boss_only = matches!(
            operation,
            "workspace.get"
                | "room.list"
                | "room.create"
                | "member.add"
                | "member.configure"
                | "member.remove"
                | "approval.decide"
        );
        if boss_only && actor != "boss" {
            return Err(error("FORBIDDEN", "boss required"));
        }
        match operation {
            "workspace.get" => Ok(
                json!({"root":self.root,"name":self.root.file_name().unwrap_or_default().to_string_lossy(),"id":hash(&self.root.to_string_lossy())}),
            ),
            "room.list" => Ok(json!(
                self.rows("SELECT * FROM rooms ORDER BY created DESC,id DESC", &[])?
            )),
            "room.create" => {
                let title = text(args, "title")?;
                if title.chars().count() > 120 || title.chars().any(char::is_control) {
                    return Err(error(
                        "INVALID_INPUT",
                        "title must be at most 120 visible characters",
                    ));
                }
                let room = id();
                self.db.execute(
                    "INSERT INTO rooms(id,title,created) VALUES(?,?,?)",
                    params![room, title, now()],
                )?;
                for (name, role) in [
                    ("你", "boss"),
                    ("协调者", "coordinator"),
                    ("执行者 1", "executor"),
                ] {
                    self.db.execute(
                        "INSERT INTO members(id,room_id,name,role,state) VALUES(?,?,?,?,?)",
                        params![
                            id(),
                            room,
                            name,
                            role,
                            if role == "boss" {
                                "ready"
                            } else {
                                "configured"
                            }
                        ],
                    )?;
                }
                self.system(&room, "工作群已创建")?;
                self.room(&room)
            }
            "room.get" => {
                let room = text(args, "room")?;
                self.room_access(actor, room)?;
                Ok(
                    json!({"room":self.room(room)?,"members":self.rows(&format!("SELECT {MEMBER_COLUMNS} FROM members WHERE room_id=? ORDER BY rowid"),&[&room])?,
                    "work":self.rows("SELECT * FROM work WHERE room_id=? ORDER BY rowid",&[&room])?,
                    "approvals":self.rows("SELECT a.* FROM approvals a JOIN members m ON m.id=a.member_id WHERE m.room_id=? AND a.decision IS NULL",&[&room])?}),
                )
            }
            "inspect" => {
                if actor == "boss" {
                    return Err(error("FORBIDDEN", "member context required"));
                }
                let member = self.member(actor)?;
                self.db
                    .execute("UPDATE members SET inspected=1 WHERE id=?", [actor])?;
                Ok(
                    json!({"member":member,"group":self.handle(actor,"room.get",&json!({"room":member["room_id"]}))?,
                    "unconfirmed":self.rows("SELECT m.* FROM messages m JOIN deliveries d ON d.message_id=m.id WHERE d.member_id=? AND d.state='delivered' ORDER BY m.seq LIMIT 100",&[&actor])?}),
                )
            }
            "skill.read" => {
                if actor == "boss" {
                    return Err(error("FORBIDDEN", "member context required"));
                }
                self.db
                    .execute("UPDATE members SET skill_read=1 WHERE id=?", [actor])?;
                Ok(
                    json!({"version":"1","content":include_str!("../../skills/sessionbridge-collaboration/SKILL.md")}),
                )
            }
            "history" => self.history(actor, args),
            "member.add" => {
                let room = text(args, "room")?;
                self.active(room)?;
                let name = text(args, "name")?;
                if name.chars().count() > 48 || name.chars().any(char::is_control) {
                    return Err(error(
                        "INVALID_INPUT",
                        "member name must be at most 48 visible characters",
                    ));
                }
                let member = id();
                self.db.execute(
                    "INSERT INTO members(id,room_id,name,role) VALUES(?,?,?,'executor')",
                    params![member, room, name],
                )?;
                self.notify_coordinator(room, &format!("新增执行者 {name}，尚未初始化"))?;
                self.member(&member)
            }
            "member.configure" => {
                let member = text(args, "member")?;
                let m = self.member(member)?;
                if m["role"] == "boss"
                    || !matches!(m["state"].as_str(), Some("configured" | "offline"))
                {
                    return Err(error(
                        "MEMBER_RUNNING",
                        "stop member before changing configuration",
                    ));
                }
                let cli = text(args, "cli")?;
                let provider = text(args, "provider")?;
                let model = text(args, "model")?;
                if model.len() > 256 || model.chars().any(char::is_control) {
                    return Err(error("INVALID_INPUT", "invalid model identifier"));
                }
                if cli != m["cli"] || provider != m["provider"] || model != m["model"] {
                    self.db.execute("UPDATE deliveries SET state='cancelled' WHERE member_id=? AND state!='processed'",[member])?;
                }
                if !["claude", "codex"].contains(&cli) {
                    return Err(error("INVALID_INPUT", "unsupported CLI"));
                }
                self.db.execute("UPDATE members SET cli=?,provider=?,model=?,session=NULL,error=NULL WHERE id=?",params![cli,provider,model,member])?;
                self.system(
                    text(&m, "room_id")?,
                    &format!(
                        "{} 的配置已更新，下次初始化将建立新的 CLI 会话",
                        text(&m, "name")?
                    ),
                )?;
                self.member(member)
            }
            "member.remove" => {
                let member = text(args, "member")?;
                let m = self.member(member)?;
                if m["role"] != "executor" {
                    return Err(error("FORBIDDEN", "only executors can be removed"));
                }
                if !matches!(m["state"].as_str(), Some("configured" | "offline")) {
                    return Err(error(
                        "MEMBER_RUNNING",
                        "stop member and wait for exit before removing",
                    ));
                }
                self.db.execute(
                    "UPDATE members SET state='removed',token_hash=NULL WHERE id=?",
                    [member],
                )?;
                self.db.execute("UPDATE deliveries SET state='cancelled' WHERE member_id=? AND state!='processed'",[member])?;
                self.db.execute("UPDATE work SET state='blocked',version=version+1 WHERE owner=? AND state IN ('running','todo')",[member])?;
                self.notify_coordinator(
                    text(&m, "room_id")?,
                    &format!("成员 {} 已移除，请重新安排其未完成工作", text(&m, "name")?),
                )?;
                Ok(json!({"removed":member}))
            }
            "ready" => {
                if actor == "boss" {
                    return Err(error("FORBIDDEN", "member context required"));
                }
                let changed=self.db.execute("UPDATE members SET state='ready',challenge=NULL,error=NULL WHERE id=? AND state='starting' AND challenge=? AND generation=? AND inspected=1 AND skill_read=1",
                    params![actor,text(args,"challenge")?,args["generation"].as_i64().ok_or_else(||error("INVALID_INPUT","generation required"))?])?;
                if args["protocol"] != "2" || changed != 1 {
                    return Err(error(
                        "HANDSHAKE",
                        "identity, challenge, protocol or capability checks failed",
                    ));
                }
                self.db.execute(
                    "UPDATE deliveries SET state='queued' WHERE member_id=? AND state='delivered'",
                    [actor],
                )?;
                let m = self.member(actor)?;
                self.notify_coordinator(
                    text(&m, "room_id")?,
                    &format!("{} 已就绪", text(&m, "name")?),
                )?;
                Ok(json!({"ready":true}))
            }
            "message.send" => self.send(actor, args),
            "message.ack" => {
                if actor == "boss" {
                    return Err(error("FORBIDDEN", "member context required"));
                }
                let ids = args["messages"]
                    .as_array()
                    .filter(|ids| !ids.is_empty() && ids.len() <= 200)
                    .ok_or_else(|| error("INVALID_INPUT", "messages required"))?;
                for mid in ids {
                    let mid = mid
                        .as_str()
                        .ok_or_else(|| error("INVALID_INPUT", "message ID must be string"))?;
                    if self.db.execute("UPDATE deliveries SET state='processed' WHERE message_id=? AND member_id=? AND state IN ('delivered','processed')",params![mid,actor])?!=1 { return Err(error("DELIVERY","message was not delivered to this member")); }
                }
                Ok(json!({"processed":ids}))
            }
            "room.transition" => self.transition(text(args, "room")?, text(args, "state")?),
            "work.create" | "work.update" => self.work(actor, operation, args),
            "approval.decide" => {
                let decision = text(args, "decision")?;
                if !["allow", "deny"].contains(&decision) {
                    return Err(error("INVALID_INPUT", "invalid decision"));
                }
                if self.db.execute(
                    "UPDATE approvals SET decision=? WHERE id=? AND decision IS NULL",
                    params![decision, text(args, "approval")?],
                )? != 1
                {
                    return Err(error("NOT_FOUND", "approval is no longer pending"));
                }
                Ok(json!({"decision":decision}))
            }
            _ => Err(error("UNKNOWN_OPERATION", operation)),
        }
    }
    fn history(&self, actor: &str, args: &Value) -> Result<Value> {
        let room = text(args, "room")?;
        self.room_access(actor, room)?;
        let limit = optional_integer(args, "limit", 100)?;
        if !(1..=200).contains(&limit) {
            return Err(error("INVALID_INPUT", "limit must be between 1 and 200"));
        }
        for field in ["after", "before", "change_after"] {
            optional_integer(args, field, 0)?;
        }
        if args.get("tail").is_some_and(|v| !v.is_boolean()) {
            return Err(error("INVALID_INPUT", "tail must be a boolean"));
        }
        if ["after", "before", "change_after"]
            .iter()
            .filter(|k| args.get(**k).is_some())
            .count()
            + usize::from(args["tail"] == true)
            > 1
        {
            return Err(error("INVALID_INPUT", "select one history cursor mode"));
        }
        let latest: i64 = self.db.query_row(
            "SELECT COALESCE(MAX(seq),0) FROM message_changes",
            [],
            |r| r.get(0),
        )?;
        let changes = args["change_after"].as_i64();
        let descending = args["tail"] == true || args.get("before").is_some();
        let after = optional_integer(args, "after", 0)?;
        let entries = if let Some(after) = changes {
            self.rows("SELECT c.seq,c.message_id AS id FROM message_changes c JOIN messages m ON m.id=c.message_id WHERE m.room_id=? AND c.seq>? ORDER BY c.seq LIMIT ?",&[&room,&after,&limit])?
        } else if descending {
            let before = optional_integer(args, "before", i64::MAX)?;
            self.rows(
                "SELECT seq,id FROM messages WHERE room_id=? AND seq<? ORDER BY seq DESC LIMIT ?",
                &[&room, &before, &limit],
            )?
        } else {
            self.rows(
                "SELECT seq,id FROM messages WHERE room_id=? AND seq>? ORDER BY seq LIMIT ?",
                &[&room, &after, &limit],
            )?
        };
        let mut messages = Vec::new();
        let mut deliveries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut bytes = 0;
        let mut change_cursor = changes.unwrap_or(latest);
        let mut has_more = entries.len() == limit as usize;
        for entry in entries {
            let mid = text(&entry, "id")?;
            let seq = entry["seq"]
                .as_i64()
                .ok_or_else(|| error("DATABASE", "invalid message cursor"))?;
            if !seen.contains(mid) {
                let message=self.rows("SELECT m.*,CASE WHEN m.sender='system' THEN '系统' ELSE n.name END AS sender_name FROM messages m LEFT JOIN members n ON n.id=m.sender WHERE m.id=?",&[&mid])?
                    .pop().ok_or_else(||error("DATABASE","message disappeared"))?;
                let addressed =
                    self.rows("SELECT * FROM deliveries WHERE message_id=?", &[&mid])?;
                let size =
                    serde_json::to_vec(&message)?.len() + serde_json::to_vec(&addressed)?.len() + 2;
                if bytes + size > crate::bridge::ipc::MAX_FRAME_BYTES - 4096 {
                    if messages.is_empty() {
                        return Err(error(
                            "FRAME_TOO_LARGE",
                            "one message exceeds the transport frame limit",
                        ));
                    }
                    has_more = true;
                    break;
                }
                bytes += size;
                seen.insert(mid.to_owned());
                messages.push(message);
                deliveries.extend(addressed);
            }
            if changes.is_some() {
                change_cursor = seq;
            }
        }
        if changes.is_some() && !has_more {
            change_cursor = latest;
        }
        messages.sort_by_key(|m| m["seq"].as_i64());
        let cursor = messages
            .last()
            .and_then(|m| m["seq"].as_i64())
            .unwrap_or(after);
        Ok(
            json!({"messages":messages,"deliveries":deliveries,"cursor":cursor,"change_cursor":change_cursor,"has_more":has_more}),
        )
    }
    fn send(&self, actor: &str, args: &Value) -> Result<Value> {
        if args.get("broadcast").is_some_and(|v| !v.is_boolean()) {
            return Err(error("INVALID_INPUT", "broadcast must be a boolean"));
        }
        if args["broadcast"] == true
            && args
                .get("recipients")
                .is_some_and(|v| !v.as_array().is_some_and(Vec::is_empty))
        {
            return Err(error(
                "INVALID_INPUT",
                "select broadcast or explicit recipients",
            ));
        }
        let room = text(args, "room")?;
        self.room_access(actor, room)?;
        if matches!(
            self.room(room)?["state"].as_str(),
            Some("cancelled" | "archived")
        ) {
            return Err(error("ROOM_INACTIVE", "room is read-only"));
        }
        let sender = if actor == "boss" {
            self.db.query_row(
                "SELECT id FROM members WHERE room_id=? AND role='boss'",
                [room],
                |r| r.get::<_, String>(0),
            )?
        } else {
            actor.into()
        };
        let kind = text(args, "kind")?;
        if !["request", "reply", "notice"].contains(&kind) {
            return Err(error("INVALID_INPUT", "invalid message kind"));
        }
        let mut recipients: Vec<String> = if args["broadcast"] == true {
            self.rows(
                "SELECT id FROM members WHERE room_id=? AND state!='removed' AND id!=?",
                &[&room, &sender],
            )?
            .iter()
            .map(|m| text(m, "id").map(str::to_owned))
            .collect::<Result<_>>()?
        } else {
            serde_json::from_value(args["recipients"].clone())?
        };
        recipients.sort();
        recipients.dedup();
        if recipients.is_empty() || recipients.len() > 100 {
            return Err(error("INVALID_INPUT", "select 1–100 recipients"));
        }
        for recipient in &recipients {
            let m = self.member(recipient)?;
            if m["room_id"] != room || m["state"] == "removed" || recipient == &sender {
                return Err(error(
                    "RECIPIENT",
                    "recipient unavailable or in another room",
                ));
            }
        }
        let reply = optional_text(args, "reply_to")?;
        let mut depth = 0;
        if let Some(reply) = reply {
            let parent = self
                .rows(
                    "SELECT * FROM messages WHERE id=? AND room_id=?",
                    &[&reply, &room],
                )?
                .pop()
                .ok_or_else(|| error("REPLY", "reply target not in room"))?;
            depth = if actor == "boss" {
                0
            } else {
                parent["depth"]
                    .as_i64()
                    .and_then(|n| n.checked_add(1))
                    .ok_or_else(|| error("DATABASE", "invalid message depth"))?
            };
        }
        if actor != "boss" && reply.is_none() {
            depth=self.db.query_row("SELECT COALESCE(MAX(m.depth),-1)+1 FROM messages m JOIN deliveries d ON d.message_id=m.id WHERE d.member_id=? AND d.state='delivered'",[actor],|r|r.get(0))?;
        }
        if depth >= 16 {
            return Err(error(
                "CHAIN_LIMIT",
                "automatic exchange limit reached; ask the boss to continue",
            ));
        }
        let result = self.insert_message(
            room,
            &sender,
            kind,
            text(args, "body")?,
            &recipients,
            reply,
            depth,
        )?;
        if actor == "boss" && kind == "request" {
            let direct_executor = recipients
                .iter()
                .any(|r| self.member(r).is_ok_and(|m| m["role"] == "executor"));
            if direct_executor {
                self.notify_coordinator(room, "老板向执行者发送了新请求，请检查并登记相关工作项")?;
            }
        }
        Ok(result)
    }
    fn work(&self, actor: &str, operation: &str, args: &Value) -> Result<Value> {
        let room = text(args, "room")?;
        self.room_access(actor, room)?;
        self.active(room)?;
        if operation == "work.create" {
            self.coordinator(actor, room)?;
            let owner = text(args, "owner")?;
            let title = text(args, "title")?;
            if title.chars().count() > 200 || title.chars().any(char::is_control) {
                return Err(error("INVALID_INPUT", "invalid work title"));
            }
            let dependencies: Vec<String> =
                serde_json::from_value(args.get("dependencies").cloned().unwrap_or(json!([])))?;
            if dependencies.len() > 32 {
                return Err(error(
                    "INVALID_INPUT",
                    "at most 32 dependencies are allowed",
                ));
            }
            let unique: std::collections::HashSet<_> = dependencies.iter().collect();
            if unique.len() != dependencies.len() {
                return Err(error(
                    "INVALID_INPUT",
                    "duplicate dependencies are not allowed",
                ));
            }
            let member = self.member(owner)?;
            if member["room_id"] != room
                || member["role"] != "executor"
                || member["state"] == "removed"
            {
                return Err(error("OWNER", "owner must be an executor in this room"));
            }
            for dep in &dependencies {
                if self
                    .rows(
                        "SELECT id FROM work WHERE id=? AND room_id=?",
                        &[dep, &room],
                    )?
                    .is_empty()
                {
                    return Err(error("DEPENDENCY", "dependency not in this room"));
                }
            }
            // Dependencies only reference existing work and are immutable: cycles cannot be introduced.
            let wid = id();
            self.db.execute(
                "INSERT INTO work(id,room_id,title,owner,dependencies) VALUES(?,?,?,?,?)",
                params![
                    wid,
                    room,
                    title,
                    owner,
                    serde_json::to_string(&dependencies)?
                ],
            )?;
            self.insert_message(
                room,
                actor,
                "request",
                &format!(
                    "工作项 {wid}: {}。先检查依赖，再通过 work.update 接单。",
                    text(args, "title")?
                ),
                &[owner.into()],
                None,
                0,
            )?;
            return Ok(json!({"id":wid,"version":1}));
        }
        let wid = text(args, "work")?;
        let old = self
            .rows(
                "SELECT * FROM work WHERE id=? AND room_id=?",
                &[&wid, &room],
            )?
            .pop()
            .ok_or_else(|| error("NOT_FOUND", "work item not found"))?;
        let coordinator = actor == "boss" || self.member(actor)?["role"] == "coordinator";
        if !coordinator && old["owner"] != actor {
            return Err(error("FORBIDDEN", "not the work owner"));
        }
        let state = text(args, "state")?;
        let from = text(&old, "state")?;
        let allowed = match (from, state) {
            ("todo" | "blocked", "running") => true,
            ("running", "review" | "blocked") => true,
            ("review", "done" | "todo") => coordinator,
            (_, "cancelled") => coordinator,
            ("done", "todo") => coordinator,
            ("blocked" | "todo" | "running", "todo") => coordinator,
            _ => false,
        };
        if !allowed {
            return Err(error("WORK_STATE", "invalid work transition"));
        }
        let owner = optional_text(args, "owner")?.unwrap_or(text(&old, "owner")?);
        if owner != text(&old, "owner")? || (from == "running" && state == "todo") {
            self.coordinator(actor, room)?;
            let previous = self.member(text(&old, "owner")?)?;
            if from == "running"
                && !matches!(previous["state"].as_str(), Some("offline" | "removed"))
            {
                return Err(error(
                    "OWNER_RUNNING",
                    "stop the previous executor before reassigning running work",
                ));
            }
            let next = self.member(owner)?;
            if next["room_id"] != room
                || next["role"] != "executor"
                || next["state"] == "removed"
                || state != "todo"
            {
                return Err(error(
                    "OWNER",
                    "reassignment requires an executor in the room and todo state",
                ));
            }
        }
        if matches!(state, "running" | "review" | "done") {
            let dependencies = old["dependencies"]
                .as_array()
                .ok_or_else(|| error("DATABASE", "invalid work dependencies"))?;
            for dep in dependencies {
                let dependency = dep
                    .as_str()
                    .ok_or_else(|| error("DATABASE", "invalid work dependency ID"))?;
                let state: String =
                    self.db
                        .query_row("SELECT state FROM work WHERE id=?", [dependency], |r| {
                            r.get(0)
                        })?;
                if state != "done" {
                    return Err(error("DEPENDENCY", "dependency is not accepted"));
                }
            }
        }
        if state == "running" {
            let busy: i64 = self.db.query_row(
                "SELECT count(*) FROM work WHERE owner=? AND state='running'",
                [text(&old, "owner")?],
                |r| r.get(0),
            )?;
            if busy > 0 {
                return Err(error("OWNER_BUSY", "executor already has running work"));
            }
            let member = self.member(owner)?;
            let checkout = crate::git::GitRepository::open(text(&member, "cwd")?)?;
            checkout.require_clean()?;
            let group = self.room(room)?;
            let integration = crate::git::GitRepository::open(text(&group, "integration_path")?)?;
            if checkout.head_commit()? != integration.head_commit()? {
                return Err(error(
                    "WORKTREE_BASELINE",
                    "preserve previous work on its branch, then start a clean branch at the current integration HEAD",
                ));
            }
        }
        let evidence = optional_text(args, "evidence")?.unwrap_or("");
        if matches!(state, "review" | "done" | "blocked") && evidence.trim().is_empty() {
            return Err(error("EVIDENCE", "evidence or blocking reason is required"));
        }
        if state != "review" && args.get("commit").is_some() {
            return Err(error(
                "INVALID_INPUT",
                "commit can only be submitted for review",
            ));
        }
        if state == "review" {
            let commit = text(args, "commit")?;
            if commit.len() != 40 || !commit.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(error("COMMIT", "full commit SHA required"));
            }
            let owner = self.member(text(&old, "owner")?)?;
            let repo = crate::git::GitRepository::open(text(&owner, "cwd")?)?;
            repo.require_clean()?;
            repo.require_ancestor(commit, "HEAD")?;
        }
        if state == "done" {
            let room = self.room(room)?;
            let repo = crate::git::GitRepository::open(text(&room, "integration_path")?)?;
            repo.require_clean()?;
            repo.require_ancestor(text(&old, "commit_id")?, "HEAD")?;
        }
        let version = args["version"]
            .as_i64()
            .ok_or_else(|| error("VERSION", "expected work version required"))?;
        if self.db.execute("UPDATE work SET state=?,owner=?,evidence=?,commit_id=CASE WHEN ?='todo' THEN NULL ELSE COALESCE(?,commit_id) END,version=version+1 WHERE id=? AND version=?",params![state,owner,evidence,state,args["commit"].as_str(),wid,version])?!=1 {return Err(error("CONFLICT","work changed; inspect and retry"));}
        if state == "todo" {
            let dependents=self.rows("WITH RECURSIVE affected(id) AS (
                SELECT w.id FROM work w,json_each(w.dependencies) d WHERE w.room_id=?1 AND d.value=?2
                UNION SELECT w.id FROM work w,json_each(w.dependencies) d JOIN affected a ON d.value=a.id WHERE w.room_id=?1
                ) SELECT id,owner FROM work WHERE id IN (SELECT id FROM affected) AND state!='cancelled'",&[&room,&wid])?;
            for dependent in dependents {
                self.db.execute("UPDATE work SET state=CASE WHEN state='running' THEN 'blocked' ELSE 'todo' END,commit_id=NULL,version=version+1,evidence='上游工作已更新，需要重新验证' WHERE id=?",[text(&dependent,"id")?])?;
                self.insert_message(
                    room,
                    actor,
                    "notice",
                    "上游工作已更新，请停止旧版本实施，检查工作项和依赖。",
                    &[text(&dependent, "owner")?.into()],
                    None,
                    0,
                )?;
            }
            self.insert_message(
                room,
                actor,
                "request",
                &format!("工作项 {wid} 已重新分配，请检查最新要求与依赖"),
                &[owner.into()],
                None,
                0,
            )?;
        }
        self.notify_coordinator(
            room,
            &format!("工作项 {} → {state}\n{evidence}", text(&old, "title")?),
        )?;
        Ok(json!({"id":wid,"state":state,"version":version+1}))
    }
}
