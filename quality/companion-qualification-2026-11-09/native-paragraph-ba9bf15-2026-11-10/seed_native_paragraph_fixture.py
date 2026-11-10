#!/usr/bin/env python3
r"""Seed synthetic paragraph QA only into a fresh, marked M5 fixture.

Python standard library only. This is a disposable QA artifact, not an app
migration. `snapshot` opens read-only. `seed` never creates a profile/database.

Windows compatibility: the Rust initializer canonicalizes its root, and core
only trims workspace strings before persisting them. Thus stored workspaces
can have a \\?\ prefix. Existing paths are compared by samefile identity plus
their resolved path forms; the extended prefix is representation, not a new
allowed root. Reparse/containment and write-authorizer guards remain in force.
"""

from __future__ import annotations

import argparse
from contextlib import closing
from datetime import datetime, timedelta, timezone
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import sqlite3
import sys
from urllib.parse import urlsplit


MARKER = b"pulsation-ui-fixture-v1\n"
PREFIX = "native-paragraph-qa-2026-11-09-"
CONVERSATIONS = tuple(PREFIX + str(index) for index in range(1, 4))
TITLES = (
    "QA nativo — Parágrafos e seleção",
    "QA nativo — Tabela, código e Unicode",
    "QA nativo — Diff e limites de quebra",
)
DRAFT_UNICODE = (
    "Rascunho Unicode não enviado — ação, coração, café; Ω, 漢字, 🙂.\n\n"
    "Selecionar e copiar preserva os parágrafos, a acentuação e os espaços.\n"
    "Segunda linha do rascunho: a + b → c; e\u0301 = é visualmente."
)
MESSAGES = (
    (
        "## Leitura e seleção\n\n"
        "Este pedido é conteúdo sintético de QA. Observe a distância entre os "
        "parágrafos quando a janela muda de largura, sem enviar nada.\n\n"
        "Ação, coração, café, São Paulo — Ω, 漢字 e 🙂 devem permanecer completos. "
        "A seleção começa nesta linha e pode terminar no último parágrafo.\n\n"
        "Último parágrafo do pedido: copiar deve manter as linhas em branco.",
        "## Resposta sintética\n\n"
        "O primeiro parágrafo usa **negrito**, *ênfase* e `código inline` para "
        "exercitar a tipografia dentro do contêiner de texto selecionável.\n\n"
        "O segundo parágrafo é deliberadamente mais longo: ao redimensionar "
        "a janela, palavras completas e acentos devem seguir as quebras nativas, "
        "preservando a altura acumulada e a distância até a próxima linha.\n\n"
        "Português: ação e coração. Unicode: Ω, 漢字, 🙂, e\u0301. "
        "Confira a seleção nos dois sentidos, inclusive no espaço inferior.\n\n"
        "Fim da resposta sintética. Nenhum modelo produziu este texto.",
    ),
    (
        "## Conteúdo misto\n\n"
        "Compare os parágrafos antes e depois da tabela, inclusive com a "
        "largura reduzida do Companion. Este histórico é inteiramente sintético.\n\n"
        "A sequência café → ação → Ω → 漢字 → 🙂 é parte do texto de teste.\n\n"
        "Mantenha o rascunho da primeira conversa sem envio.",
        "## Tabela de inspeção\n\n"
        "Parágrafo anterior à tabela: a largura das células não deve alterar "
        "a seleção dos parágrafos que a cercam.\n\n"
        "| Caso | Texto | Esperado |\n"
        "| --- | --- | --- |\n"
        "| Acentos | ação, café | caracteres completos |\n"
        "| Unicode | Ω 漢字 🙂 | cópia preservada |\n"
        "| Espaçamento | dois parágrafos | linha em branco |\n\n"
        "Parágrafo após a tabela, com **destaque** e `valor = 42`.\n\n"
        "```rust\n"
        "// Exemplo visual; não executar.\n"
        "let mensagem = \"ação — Ω — 漢字 — 🙂\";\n"
        "assert!(!mensagem.is_empty());\n"
        "```\n\n"
        "Último parágrafo: volte ao início e confira a cópia completa.",
    ),
    (
        "## Quebras e diff\n\n"
        "Inspecione as margens deste pedido e da resposta em Chat completo. "
        "Depois use o Companion para conferir os outros tópicos.\n\n"
        "Uma palavra longa ajuda a observar os limites: "
        "paragrafo_unicode_selecao_redimensionamento_0123456789_Ω_漢字.\n\n"
        "Tudo aqui é dados de teste; não há ação a aprovar.",
        "## Diff sintético\n\n"
        "Primeiro parágrafo da resposta: observe a altura e as quebras ao "
        "alternar entre Chat e Companion e ao redimensionar a janela.\n\n"
        "```diff\n"
        "--- a/qa-sintetico.txt\n"
        "+++ b/qa-sintetico.txt\n"
        "@@ -1,2 +1,3 @@\n"
        "-Parágrafo antigo\n"
        "+Parágrafo novo — ação, café, Ω\n"
        "+Unicode preservado: 漢字 🙂\n"
        " Linha mantida\n"
        "```\n\n"
        "Este parágrafo aparece após o diff. A cópia deve preservar seu "
        "texto e o espaço entre blocos. **Negrito** e *ênfase* usam o mesmo fluxo.\n\n"
        "> Citação sintética: nenhuma aprovação ou envio decorre desta leitura.\n\n"
        "Último parágrafo: seleção para trás e para frente deve alcançar "
        "todos os caracteres até este ponto final.",
    ),
)
INSERT_TABLES = {
    "chat_conversations", "chat_messages", "companion_dock_sessions",
    "companion_drafts",
}
UI_COLUMNS = {"active_view", "active_chat_conversation_id", "updated_at"}


class Refusal(Exception):
    pass


def require(condition: bool, message: str) -> None:
    if not condition:
        raise Refusal(message)


def comparable_path(path: Path) -> Path:
    """Normalize only equivalent Windows extended drive/UNC spellings.

    This does not resolve links or grant containment. Callers must resolve first
    before using it for a safety decision. GLOBALROOT/device paths are not
    rewritten. A resolved destination outside target/ still fails containment.
    """
    if os.name != "nt":
        return path
    text = str(path)
    if text.startswith("\\\\?\\UNC\\"):
        return Path("\\\\" + text[8:])
    if (text.startswith("\\\\?\\") and len(text) >= 7
            and text[4].isalpha() and text[5:7] == ":\\"):
        return Path(text[4:])
    return path


def resolved_path(path: Path) -> Path:
    resolved = path.resolve(strict=True)
    comparable = comparable_path(resolved)
    # Extended syntax can change Windows filename semantics (for example,
    # trailing spaces). Only use the normal spelling when it is the same object.
    require(resolved.samefile(comparable), "Windows path spelling changed its destination.")
    return comparable


def same_existing_path(stored: str | Path, expected: Path) -> bool:
    actual = Path(stored)
    # Path.resolve preserves an existing \\?\ prefix on Windows; lexical Path
    # equality alone would reject the initializer's valid canonicalized paths.
    return (actual.samefile(expected)
            and resolved_path(actual) == resolved_path(expected))


def paths(requested: str) -> tuple[Path, Path]:
    # Fixed artifact placement anchors the allowed tree; no --db or --target override.
    repo = resolved_path(Path(__file__)).parents[3]
    require(repo.name == "HARD-VC" and (repo / "docs/architecture.md").is_file(),
            "Script must remain in its original HARD-VC/target artifact directory.")
    lexical_target = repo / "target"
    target = resolved_path(lexical_target)
    require(target == lexical_target, "Refuse a redirected target directory.")
    root = resolved_path(Path(requested))
    require(root != target and root.is_relative_to(target),
            "Profile root must resolve strictly below this repository's target/.")
    require(root.is_dir(), "Profile root must already exist; run the Rust fixture first.")
    marker = resolved_path(root / "M5_UI_FIXTURE")
    require(marker.parent == root and marker.read_bytes() == MARKER,
            "Refuse an absent, redirected or non-exact M5_UI_FIXTURE marker.")
    data = resolved_path(root / "data")
    require(data == root / "data" and data.is_dir(), "Refuse redirected data directory.")
    database = resolved_path(data / "hard-vibe-code.db")
    require(database == data / "hard-vibe-code.db" and database.is_file()
            and database.stat().st_nlink == 1,
            "Refuse redirected or absent fixture database.")
    with database.open("rb") as file:
        require(file.read(16) == b"SQLite format 3\x00", "Not a real SQLite database.")
    # SQLite may write these companions when the explicit mode=rw transaction commits.
    for suffix in ("-wal", "-shm", "-journal"):
        sidecar = Path(str(database) + suffix)
        require(not os.path.lexists(sidecar)
                or (resolved_path(sidecar) == sidecar and sidecar.is_file()
                    and sidecar.stat().st_nlink == 1),
                "Refuse redirected SQLite sidecar.")
    return root, database


def connect(database: Path, writable: bool) -> sqlite3.Connection:
    # mode=rw refuses creation. Only paths() can supply a database path here.
    mode = "rw" if writable else "ro"
    db = sqlite3.connect(database.as_uri() + "?mode=" + mode,
                         uri=True, timeout=5, isolation_level=None)
    db.row_factory = sqlite3.Row
    db.enable_load_extension(False)
    db.execute("PRAGMA foreign_keys=ON")
    if not writable:
        db.execute("PRAGMA query_only=ON")
    opened = Path(db.execute("PRAGMA database_list").fetchone()[2])
    require(same_existing_path(opened, database), "SQLite opened an unexpected database.")
    return db


def validate(db: sqlite3.Connection, root: Path, fresh: bool) -> sqlite3.Row:
    require((root / "M5_UI_FIXTURE").read_bytes() == MARKER, "Fixture marker changed.")
    require(db.execute("PRAGMA quick_check").fetchall()[0][0] == "ok",
            "SQLite quick_check failed.")
    require(not db.execute("PRAGMA foreign_key_check").fetchall(),
            "Fixture has foreign-key violations.")
    projects = db.execute("SELECT * FROM projects ORDER BY name").fetchall()
    require(len(projects) == 2 and {p["name"] for p in projects} ==
            {"M5 Principal", "M5 Segundo"}, "Refuse a non-M5 project set.")
    for project in projects:
        expected = resolved_path(root / project["name"])
        require(expected == root / project["name"] and expected.is_dir()
                and same_existing_path(project["workspace_path"], expected)
                and project["is_archived"] == 0, "Fixture workspace is not isolated.")
    project = next(p for p in projects if p["name"] == "M5 Segundo")
    latest = db.execute("""SELECT id FROM projects WHERE is_archived=0 ORDER BY
        CASE WHEN last_opened_at IS NULL THEN 1 ELSE 0 END,
        last_opened_at DESC, updated_at DESC LIMIT 1""").fetchone()
    require(latest is not None and latest[0] == project["id"],
            "M5 Segundo must remain the initializer's last active project.")
    connections = db.execute("SELECT * FROM provider_connections").fetchall()
    require(bool(connections), "Missing fixture provider connections.")
    require(all(c["credential_ref"] == "m5-ui-fixture:" + c["id"]
                and c["enabled"] in (0, 1) for c in connections),
            "Unsafe credential reference or enabled flag.")
    enabled = [c for c in connections if c["enabled"] == 1]
    require(len(enabled) == 1 and enabled[0]["id"] == "legacy-openai"
            and enabled[0]["provider_id"] == "ollama"
            and enabled[0]["state"] == "active"
            and bool(enabled[0]["default_model"]), "Unsafe active provider connection.")
    endpoint = urlsplit(enabled[0]["endpoint_url"] or "")
    try:
        host = ipaddress.ip_address(endpoint.hostname or "")
        safe_endpoint = (endpoint.scheme == "http" and host.is_loopback
                         and endpoint.port is not None and endpoint.port > 0
                         and endpoint.username is None and endpoint.password is None
                         and endpoint.path in ("", "/")
                         and not endpoint.query and not endpoint.fragment)
    except ValueError:
        safe_endpoint = False
    require(safe_endpoint, "Fixture endpoint must be an explicit HTTP loopback IP and port.")
    # V3: runtime_settings.value TEXT; configure_loopback writes this exact key
    # to the text value 'off'. V5 provider_connections.endpoint_url is TEXT;
    # the current HTTP fixture binds 127.0.0.1:0 and url("") supplies HTTP /.
    routing = db.execute("SELECT value FROM runtime_settings WHERE key='runtime.routing_v2_mode'").fetchone()
    require(routing is not None and routing[0] == "off", "Fixture routing mode is unexpected.")
    session = db.execute("SELECT * FROM ui_sessions WHERE project_id=?", (project["id"],)).fetchone()
    require(session is not None and session["workspace_view_mode"] == "editor"
            and same_existing_path(session["workspace_root"], root / "M5 Segundo"),
            "Missing initializer UI session for M5 Segundo.")
    if fresh:
        require(not (root / "STOP").exists(), "Fixture has already been stopped.")
        for table in ("chat_conversations", "chat_messages", "chat_model_calls",
                      "chat_tool_calls", "companion_dock_sessions", "companion_drafts",
                      "decision_conversations"):
            require(db.execute('SELECT COUNT(*) FROM "' + table + '"').fetchone()[0] == 0,
                    "Refuse an already used profile or existing QA IDs: " + table)
        require(session["active_view"] == "pulsations"
                and session["active_chat_conversation_id"] is None,
                "Profile UI state is not the fresh initializer state.")
    return project


def quote_identifier(name: str) -> str:
    return '"' + name.replace('"', '""') + '"'


def protected_fingerprint(db: sqlite3.Connection) -> str:
    digest = hashlib.sha256()
    tables = db.execute("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name").fetchall()
    for (name,) in tables:
        if name in INSERT_TABLES or name.startswith("sqlite_"):
            continue
        rows = db.execute("SELECT * FROM " + quote_identifier(name)).fetchall()
        columns = [c[1] for c in db.execute("PRAGMA table_info(" + quote_identifier(name) + ")")]
        values = [[row[c] for c in columns if name != "ui_sessions" or c not in UI_COLUMNS]
                  for row in rows]
        encoded = sorted(json.dumps(row, ensure_ascii=False, sort_keys=True,
                                   default=lambda value: value.hex()) for row in values)
        digest.update(json.dumps([name, encoded], ensure_ascii=False).encode("utf-8"))
    return digest.hexdigest()


def authorize(action: int, arg1: str | None, arg2: str | None,
              database_name: str | None, trigger: str | None) -> int:
    if database_name not in (None, "main") or trigger is not None:
        return sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_INSERT:
        return sqlite3.SQLITE_OK if arg1 in INSERT_TABLES else sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_UPDATE:
        return sqlite3.SQLITE_OK if arg1 == "ui_sessions" and arg2 in UI_COLUMNS else sqlite3.SQLITE_DENY
    if action == sqlite3.SQLITE_PRAGMA:
        # Observed on the guarded fresh profile during a read-only fingerprint:
        # SQLITE_PRAGMA(19), data_version, None, main, None. SQLite's FTS reads
        # use this read-only counter; no argument/assignment form is allowed.
        if arg1 == "data_version" and arg2 is None and database_name == "main":
            return sqlite3.SQLITE_OK
        return sqlite3.SQLITE_OK if arg1 in {"foreign_key_check", "table_info"} else sqlite3.SQLITE_DENY
    allowed = {sqlite3.SQLITE_SELECT, sqlite3.SQLITE_READ, sqlite3.SQLITE_FUNCTION,
               sqlite3.SQLITE_TRANSACTION, sqlite3.SQLITE_SAVEPOINT}
    return sqlite3.SQLITE_OK if action in allowed else sqlite3.SQLITE_DENY


def snapshot(db: sqlite3.Connection, project: sqlite3.Row) -> dict:
    marks = ",".join("?" for _ in CONVERSATIONS)
    counts = {
        "qa_conversations": db.execute("SELECT COUNT(*) FROM chat_conversations WHERE id IN (" + marks + ")", CONVERSATIONS).fetchone()[0],
        "qa_messages": db.execute("SELECT COUNT(*) FROM chat_messages WHERE conversation_id IN (" + marks + ")", CONVERSATIONS).fetchone()[0],
        "qa_noncompleted_messages": db.execute("SELECT COUNT(*) FROM chat_messages WHERE conversation_id IN (" + marks + ") AND status<>'completed'", CONVERSATIONS).fetchone()[0],
        "qa_model_calls": db.execute("SELECT COUNT(*) FROM chat_model_calls c JOIN chat_messages m ON m.id=c.assistant_message_id WHERE m.conversation_id IN (" + marks + ")", CONVERSATIONS).fetchone()[0],
        "qa_tool_calls": db.execute("SELECT COUNT(*) FROM chat_tool_calls c JOIN chat_messages m ON m.id=c.assistant_message_id WHERE m.conversation_id IN (" + marks + ")", CONVERSATIONS).fetchone()[0],
    }
    layout_row = db.execute("SELECT layout_json FROM companion_dock_sessions WHERE project_id=?", (project["id"],)).fetchone()
    layout = json.loads(layout_row[0]) if layout_row else {}
    counts["open_docks"] = int(layout.get("open", False))
    counts["dock_topics"] = len(layout.get("conversations", []))
    counts["active_qa_chat"] = db.execute("SELECT COUNT(*) FROM ui_sessions WHERE project_id=? AND active_view='chat' AND active_chat_conversation_id=?", (project["id"], CONVERSATIONS[2])).fetchone()[0]
    for table in ("pulsations", "jobs", "checkpoints"):
        counts[table] = db.execute("SELECT COUNT(*) FROM " + table).fetchone()[0]
    drafts = db.execute("SELECT revision,content FROM companion_drafts WHERE project_id=? ORDER BY topic_id", (project["id"],)).fetchall()
    counts["drafts"] = len(drafts)
    return {"counts": counts, "drafts": [dict(draft) for draft in drafts]}


def snapshot_json(value: dict) -> str:
    # Windows exec stdout may be cp1252. ASCII JSON escapes preserve the exact
    # Unicode payload on json.loads and cannot fail only after a successful COMMIT.
    return json.dumps(value, ensure_ascii=True, indent=2)


def seed(db: sqlite3.Connection, root: Path) -> dict:
    db.execute("BEGIN IMMEDIATE")
    try:
        project = validate(db, root, fresh=True)
        protected = protected_fingerprint(db)
        placeholders = ",".join("?" for _ in INSERT_TABLES)
        triggers = db.execute("SELECT COUNT(*) FROM sqlite_schema WHERE type='trigger' AND tbl_name IN (" + placeholders + ")", tuple(INSERT_TABLES)).fetchone()[0]
        require(triggers == 0, "Refuse unexpected write-table triggers.")
        connection = db.execute("SELECT default_model FROM provider_connections WHERE id='legacy-openai'").fetchone()
        db.set_authorizer(authorize)
        now = datetime.now(timezone.utc)
        for index, conversation in enumerate(CONVERSATIONS):
            stamp = (now + timedelta(microseconds=index)).isoformat()
            db.execute("""INSERT INTO chat_conversations
                (id,project_id,connection_id,provider_id_snapshot,model_id_snapshot,
                 title,created_at,updated_at,transport_kind,cli_backend,model_profile,model_source)
                VALUES(?,?,'legacy-openai','ollama',?,?,?,?,'api',NULL,NULL,'manual')""",
                       (conversation, project["id"], connection[0], TITLES[index], stamp, stamp))
            for ordinal, role in enumerate(("user", "assistant")):
                db.execute("""INSERT INTO chat_messages
                    (id,conversation_id,ordinal,role,content,status,created_at,updated_at)
                    VALUES(?,?,?,?,?,'completed',?,?)""",
                           (conversation + "-" + role, conversation, ordinal, role,
                            MESSAGES[index][ordinal], stamp, stamp))
        layout = {"open": True, "conversations": [
            {"topicId": "chat:" + c, "conversationId": c,
             "decisionId": None, "minimized": False} for c in CONVERSATIONS]}
        stamp = now.isoformat()
        db.execute("INSERT INTO companion_dock_sessions(project_id,revision,layout_json,updated_at) VALUES(?,1,?,?)",
                   (project["id"], json.dumps(layout, ensure_ascii=False), stamp))
        db.execute("""INSERT INTO companion_drafts(topic_id,conversation_id,project_id,
            revision,content,decision_id,decision_state_version,updated_at)
            VALUES(?,?,?,1,?,NULL,NULL,?)""",
                   ("chat:" + CONVERSATIONS[0], CONVERSATIONS[0], project["id"], DRAFT_UNICODE, stamp))
        # Preserve active_pulsation_id, tabs, workspace, sidebar and every other field.
        changed = db.execute("""UPDATE ui_sessions SET active_view='chat',
            active_chat_conversation_id=?,updated_at=? WHERE project_id=?""",
                             (CONVERSATIONS[2], stamp, project["id"])).rowcount
        require(changed == 1, "Expected exactly one UI session update.")
        require(protected_fingerprint(db) == protected, "Protected fixture state changed.")
        require(not db.execute("PRAGMA foreign_key_check").fetchall(), "Foreign-key check failed.")
        result = snapshot(db, project)
        require(result["counts"]["qa_conversations"] == 3
                and result["counts"]["qa_messages"] == 6
                and result["counts"]["qa_noncompleted_messages"] == 0
                and result["counts"]["qa_model_calls"] == 0
                and result["counts"]["qa_tool_calls"] == 0
                and result["drafts"] == [{"revision": 1, "content": DRAFT_UNICODE}],
                "Seed verification failed.")
        db.execute("COMMIT")
        return result
    except BaseException:
        db.execute("ROLLBACK")
        raise


def self_check() -> None:
    # Pure assertions: no SQLite connection or profile read/write.
    require(len(set(CONVERSATIONS)) == 3 and len(TITLES) == 3 and len(MESSAGES) == 3,
            "Fixture IDs/titles/content mismatch.")
    require(all(len(pair) == 2 and all("\n\n" in text for text in pair) for pair in MESSAGES),
            "Each of six messages must contain multiple paragraphs.")
    require("| Caso |" in MESSAGES[1][1] and "```diff" in MESSAGES[2][1],
            "Expected table and diff examples.")
    require(0 < len(DRAFT_UNICODE) <= 20_000 and "🙂" in DRAFT_UNICODE
            and all(DRAFT_UNICODE not in text for pair in MESSAGES for text in pair),
            "Draft must remain separate and within the core limit.")
    require(authorize(sqlite3.SQLITE_UPDATE, "jobs", "status", "main", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_INSERT, "chat_model_calls", None, "main", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_DELETE, "chat_messages", None, "main", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_UPDATE, "ui_sessions", "active_pulsation_id", "main", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_INSERT, "chat_messages", None, "main", "unexpected_trigger") == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_PRAGMA, "data_version", "1", "main", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_PRAGMA, "data_version", None, "temp", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_PRAGMA, "journal_mode", "WAL", "main", None) == sqlite3.SQLITE_DENY
            and authorize(sqlite3.SQLITE_PRAGMA, "data_version", None, "main", None) == sqlite3.SQLITE_OK,
            "Write guard failed.")
    unicode_result = {"counts": {"drafts": 1},
                      "drafts": [{"revision": 1, "content": DRAFT_UNICODE}]}
    encoded = snapshot_json(unicode_result)
    require(json.loads(encoded.encode("cp1252").decode("cp1252")) == unicode_result
            and encoded.encode("ascii").decode("ascii") == encoded,
            "CLI JSON must preserve exact Unicode while fitting cp1252/ASCII stdout.")
    if os.name == "nt":
        normal = Path("C:/fixture/target/qa")
        extended = Path("\\\\?\\C:\\fixture\\target\\qa")
        require(comparable_path(extended) == normal
                and comparable_path(Path("\\\\?\\UNC\\server\\share\\qa"))
                == Path("\\\\server\\share\\qa")
                and not comparable_path(Path("\\\\?\\C:\\elsewhere\\qa")).is_relative_to(
                    comparable_path(Path("\\\\?\\C:\\fixture\\target"))),
                "Windows spelling normalization changed the target boundary.")
    print("Pure fixture self-check passed; no database opened.")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("seed", "snapshot", "self-check"))
    parser.add_argument("root", nargs="?", help="Existing fresh M5 root strictly below HARD-VC/target")
    args = parser.parse_args()
    if args.command == "self-check":
        require(args.root is None, "self-check takes no profile path.")
        self_check()
        return 0
    require(args.root is not None, "A fixture root is required; no default profile is used.")
    root, database = paths(args.root)
    # First validation is always RO. RW repeats it under BEGIN IMMEDIATE.
    with closing(connect(database, writable=False)) as db:
        db.execute("BEGIN")
        project = validate(db, root, fresh=args.command == "seed")
        result = snapshot(db, project) if args.command == "snapshot" else None
        db.execute("COMMIT")
    if args.command == "seed":
        root_again, db_again = paths(args.root)
        require((root_again, db_again) == (root, database), "Resolved fixture paths changed.")
        with closing(connect(database, writable=True)) as db:
            result = seed(db, root)
    # No IDs, endpoint URLs, credential refs, message bodies or paths in snapshots.
    print(snapshot_json(result))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (Refusal, OSError, sqlite3.Error, ValueError, KeyError) as error:
        print("REFUSED: " + str(error), file=sys.stderr)
        raise SystemExit(2)
