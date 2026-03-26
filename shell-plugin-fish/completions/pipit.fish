# ──────────────────────────────────────────────────────────────────────
#  Completions for the `pipit` command
# ──────────────────────────────────────────────────────────────────────
#
#  Fish's completion system is declarative:
#    complete -c command -n condition -a arguments -d description
#
#  No compdef boilerplate. Each line is self-contained and testable.
#
# ──────────────────────────────────────────────────────────────────────

# Disable file completions for pipit (we handle it ourselves)
complete -c pipit -f

# ── Top-level subcommands ──
complete -c pipit -n "__fish_use_subcommand" -a new         -d "Start new conversation"
complete -c pipit -n "__fish_use_subcommand" -a info        -d "Show session info"
complete -c pipit -n "__fish_use_subcommand" -a env         -d "Show environment & API keys"
complete -c pipit -n "__fish_use_subcommand" -a suggest     -d "Natural language → shell command"
complete -c pipit -n "__fish_use_subcommand" -a commit      -d "AI commit message + commit"
complete -c pipit -n "__fish_use_subcommand" -a commit-preview -d "Preview commit message only"
complete -c pipit -n "__fish_use_subcommand" -a model       -d "Set/show model"
complete -c pipit -n "__fish_use_subcommand" -a provider    -d "Set/show provider"
complete -c pipit -n "__fish_use_subcommand" -a agent       -d "Set/show agent"
complete -c pipit -n "__fish_use_subcommand" -a conversation -d "Switch conversations"
complete -c pipit -n "__fish_use_subcommand" -a auth        -d "Credential management"
complete -c pipit -n "__fish_use_subcommand" -a sandbox     -d "Git worktree sandbox"
complete -c pipit -n "__fish_use_subcommand" -a undo        -d "Undo file changes"
complete -c pipit -n "__fish_use_subcommand" -a todo        -d "Task tracking"
complete -c pipit -n "__fish_use_subcommand" -a data        -d "JSONL batch processing"
complete -c pipit -n "__fish_use_subcommand" -a diff        -d "Colored diff rendering"
complete -c pipit -n "__fish_use_subcommand" -a doctor      -d "Run diagnostics"
complete -c pipit -n "__fish_use_subcommand" -a copy        -d "Copy last response"
complete -c pipit -n "__fish_use_subcommand" -a keyboard    -d "Show key bindings"
complete -c pipit -n "__fish_use_subcommand" -a help        -d "Show help"

# ── pipit provider <name> ──
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "anthropic"  -d "Anthropic (Claude)"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "openai"     -d "OpenAI (GPT)"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "deepseek"   -d "DeepSeek"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "google"     -d "Google (Gemini)"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "openrouter" -d "OpenRouter"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "xai"        -d "xAI (Grok)"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "cerebras"   -d "Cerebras"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "groq"       -d "Groq"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "mistral"    -d "Mistral"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "ollama"     -d "Ollama (local)"
complete -c pipit -n "__fish_seen_subcommand_from provider" -a "reset"      -d "Clear override"

# ── pipit model <name|reset> ──
complete -c pipit -n "__fish_seen_subcommand_from model" -a "reset" -d "Clear model override"

# ── pipit agent <name|reset> ──
complete -c pipit -n "__fish_seen_subcommand_from agent" -a "reset" -d "Clear agent override"

# ── pipit conversation <-|id> ──
complete -c pipit -n "__fish_seen_subcommand_from conversation" -a "-" -d "Switch to previous conversation"

# ── pipit auth <subcommand> ──
complete -c pipit -n "__fish_seen_subcommand_from auth" -a "status" -d "Show stored credentials"
complete -c pipit -n "__fish_seen_subcommand_from auth" -a "login"  -d "Login to provider"
complete -c pipit -n "__fish_seen_subcommand_from auth" -a "logout" -d "Logout from provider"

# ── pipit auth login <provider> ──
complete -c pipit -n "__fish_seen_subcommand_from login" -a "anthropic openai deepseek google openrouter xai cerebras groq mistral ollama"

# ── pipit auth login flags ──
complete -c pipit -n "__fish_seen_subcommand_from login" -l api-key -d "Provide API key directly"
complete -c pipit -n "__fish_seen_subcommand_from login" -l device  -d "Use OAuth device flow"
complete -c pipit -n "__fish_seen_subcommand_from login" -l adc     -d "Use Google ADC"

# ── pipit sandbox <subcommand> ──
complete -c pipit -n "__fish_seen_subcommand_from sandbox" -a "exit"   -d "Return and cleanup worktree"
complete -c pipit -n "__fish_seen_subcommand_from sandbox" -a "status" -d "Show sandbox info"
complete -c pipit -n "__fish_seen_subcommand_from sandbox" -a "keep"   -d "Exit but keep worktree"

# ── pipit undo <subcommand> ──
complete -c pipit -n "__fish_seen_subcommand_from undo" -a "track" -d "Snapshot a file before editing"
complete -c pipit -n "__fish_seen_subcommand_from undo" -a "list"  -d "Show undo stack"
complete -c pipit -n "__fish_seen_subcommand_from undo" -a "clear" -d "Clear all backups"

# ── pipit todo <subcommand> ──
complete -c pipit -n "__fish_seen_subcommand_from todo" -a "add"   -d "Add a new todo"
complete -c pipit -n "__fish_seen_subcommand_from todo" -a "done"  -d "Mark todo as done"
complete -c pipit -n "__fish_seen_subcommand_from todo" -a "doing" -d "Mark todo as in-progress"
complete -c pipit -n "__fish_seen_subcommand_from todo" -a "rm"    -d "Remove a todo"
complete -c pipit -n "__fish_seen_subcommand_from todo" -a "clear" -d "Remove completed items"
complete -c pipit -n "__fish_seen_subcommand_from todo" -a "reset" -d "Remove all items"

# ── pipit suggest flags ──
complete -c pipit -n "__fish_seen_subcommand_from suggest" -l explain -d "Include explanation"
complete -c pipit -n "__fish_seen_subcommand_from suggest" -l multi   -d "Return multiple alternatives"

# ── pipit data flags ──
complete -c pipit -n "__fish_seen_subcommand_from data" -s i -l input    -rF -d "Input JSONL file"
complete -c pipit -n "__fish_seen_subcommand_from data" -s o -l output   -rF -d "Output JSONL file"
complete -c pipit -n "__fish_seen_subcommand_from data" -s t -l template -r  -d "Prompt template"
complete -c pipit -n "__fish_seen_subcommand_from data" -s p -l parallel -r  -d "Concurrent requests"
complete -c pipit -n "__fish_seen_subcommand_from data" -s f -l field    -r  -d "Output field name"

# ── pipit diff flags ──
complete -c pipit -n "__fish_seen_subcommand_from diff" -s g -l git    -d "Git diff (all changes)"
complete -c pipit -n "__fish_seen_subcommand_from diff" -s s -l staged -d "Git diff (staged only)"
