# Takiza Code — AI Coding Agent CLI

Интерактивный консольный терминальный AI-агент для разработки на **Rust**, вдохновленный **OpenClaude**, **OpenCode** (Charm Crush) и **Claude Code**.

Работает прямо в потоке терминала (scrollback), поддерживает вызов автономных инструментов, сессии диалогов, интеграцию с Git и быстрое переключение между провайдерами (OpenAI, OpenRouter, DeepSeek, Ollama, Groq).

---

## Ключевые возможности

### 🤖 Автономный цикл агента (Agent Loop)
- Итеративный цикл Function Calling: агент сам исследует репозиторий, читает файлы, вносит правки и запускает тесты.
- Анимированный спиннер (`⠋ Thinking...`, `⠋ Executing tools...`) во время рассуждений.
- Мгновенная отмена любого действия или подпроцесса по **`Ctrl+C`**.

### 🛠 Набор инструментов (Tools)
- `read_file(path, start_line, end_line)`: просмотр файлов с нумерацией строк.
- `write_file(path, content)`: создание или перезапись файлов с автосозданием родительских папок.
- `edit_file(path, target, replacement)`: точечная замена блоков с красивым цветовым diff (`-` красным, `+` зеленым).
- `list_dir(path)`: инспекция содержимого каталогов.
- `find_files(pattern, path)`: рекурсивный поиск файлов по маске/имени (игнорирует `.git`, `target`, `node_modules`).
- `grep_search(query, path)`: рекурсивный поиск текста по файлам проекта (аналог `ripgrep`).
- `run_command(command)`: выполнение bash-команд со стримингом вывода.

### 🌐 Поддержка провайдеров (`/provider`)
Быстрое переключение конфигураций:
- `openai`: OpenAI API (`https://api.openai.com/v1`, модель `gpt-4o`)
- `openrouter`: OpenRouter (`https://openrouter.ai/api/v1`, модель `anthropic/claude-3.5-sonnet`)
- `deepseek`: DeepSeek (`https://api.deepseek.com`, модель `deepseek-chat`)
- `ollama`: Локальные модели (`http://localhost:11434/v1`, модель `qwen2.5-coder:latest` / `llama3`)
- `groq`: Сверхбыстрый инференс (`https://api.groq.com/openai/v1`, модель `llama-3.3-70b-versatile`)

### 🌿 Интеграция с Git
- Автоопределение текущей ветки и статуса изменений: отображается в баннере и промпте (например, `takiza [master*] ❯ `).
- Команда `/diff`: цветной просмотр всех изменений в рабочей копии.

### 💾 Управление сессиями (Sessions)
- Автосохранение истории диалогов в `.takiza/sessions/`.
- `/status`: информация о сессии, модели, количестве сообщений и ветке Git.
- `/sessions`: список сохраненных сессий.
- `/resume <id>`: продолжение ранее сохраненной сессии.
- `/reset`: сброс истории и начало новой чистой сессии.

### Язык интерфейса и настройки

TUI поддерживает **English**, **Русский** и **简体中文** (китайский, упрощённое письмо).
На первом запуске онбординг предлагает выбрать язык, затем тему и принять соглашение.
Команда `/config` открывает общее меню настроек: язык, тема, провайдер, модель,
режим работы, глубина рассуждений и подтверждение команд. Язык применяется сразу
и сохраняется между запусками; для старых конфигураций по умолчанию используется English.

Настройки сохраняются в `.takiza/config.json`, если такой файл уже есть в рабочем
каталоге, иначе — в `~/.config/takiza/config.json`. Поле `language` принимает `en`,
`ru` или `zh`. Переписка, исходный код и вывод команд сохраняют исходный язык.
Переводы интерфейса находятся в `src/locales/ui.json`.

**English:** Select your interface language during onboarding or via `/config`.
The same menu includes theme, provider, model, mode, reasoning effort, and command
approval. Your language is saved and applied immediately; conversation content
and tool output keep their original text.

**简体中文：** 可在首次设置或 `/config` 中选择界面语言。同一菜单还可设置主题、
服务商、模型、运行模式、推理强度和命令权限。语言设置立即生效并自动保存；
会话内容、源代码和命令输出保留原文。

### ⌨️ Слэш-команды и горячие клавиши

В нижней рамке слева от названия чата показан контекст в токенах: `32K/128K`. Цвет плавно меняется от зелёного через жёлтый при заполнении наполовину к красному при заполнении целиком. При известном текущем размере и максимуме рядом показан приблизительный запас до автосжатия, например `96K/128K · ~19% до сжатия`: из окна вычитаются текущий контекст и 8K резерва ответа, остаток выражается в процентах от полного окна. На узком экране подпись сокращается до `~19%`. Это оценка по последнему usage; новые сообщения и инструменты могут её изменить. Автосжатие запускается после ошибки переполнения провайдера. Текущий размер — последний снимок usage запроса (вход + ответ), а не суммарный расход за сессию; если провайдер присылает usage во время стриминга, индикатор обновляется сразу. Максимум берётся только у выбранного провайдера через `/models`, `/models/{model}` и `/metadata`, если соответствующие endpoints доступны, либо из явного лимита в ошибке переполнения. Из нескольких сообщённых ограничений выбирается наименьшее; сведения о другой модели и её обученном окне не используются. Запросы метаданных идут в фоне, результат кэшируется до выхода; лимит из ошибки сохраняется на семь дней и доступен после перезапуска. Неизвестное значение обозначается `?`; после `/compact` текущий размер уточняется следующим запросом.

После успешных `write_file` и `edit_file` чат показывает короткое превью изменений с цветами выбранной темы. `Ctrl+O` раскрывает полный diff; изменения через `run_command` не создают такое превью.

В теме `evening-irkutsk` на свободных участках фона идёт мягкий снегопад.
Анимация не перекрывает текст и ввод, замирает при выделении текста и не попадает
в буфер обмена. Она работает в чате, меню настроек и онбординге.
Кадры обновляются примерно 60 раз в секунду; снежинки двигаются с разной скоростью,
заметным диагональным сносом и широким покачиванием. Используются снежинки
`❄︎`, `❅`, `❆`; плавное изменение яркости смягчает переходы между соседними ячейками.

| `/theme` | Выбор с предпросмотром: amber, cyberpunk, emerald, nord, monochrome, dracula, rose, ocean, sakura, solarized, evening-irkutsk. Можно указать имя напрямую: `/theme ocean` |
| `/mode` | Переключение режима работы (`Takiza Manual` или `Takiza MoA`) |
| `/help` | Справка по всем командам |
| `/clear` | Начать новый чат, сбросить контекст, историю и название |
| `Ctrl+L` | Перерисовать экран текущего чата |
| `/model <name>` | Сменить модель (например, `/model claude-3-5-sonnet` или `/model gpt-4o-mini`) |
| `/provider <name>` | Сменить пресет провайдера (`groq`, `openrouter`, `openai`, `deepseek`, `ollama`) |
| `/approval` | Переключить подтверждения команд, записи вне workspace и MCP (спрашивать/авто) |
| `/rewind`, `/restore` | Выбрать точку до запроса, просмотреть изменения и восстановить файлы проекта |
| `/compact` | Сжать контекст модели, сохранив видимую переписку |
| `/diff` | Показать git diff текущих изменений с возможностью коммита/отката |
| `/status` | Статус текущей сессии, расхода токенов и git ветки |
| `/usage` | Моковый дашборд лимитов и квот (`Takiza Manual` и `Takiza MoA`) |
| `/sessions` | Интерактивный список сохраненных диалогов |
| `/resume <id>` | Возобновить сессию по ID |
| `/reset` | Начать новый диалог с чистого листа |
| `/tools` | Интерактивный список активных инструментов агента |
| `/skills` | Поиск локальных и глобальных скиллов; фильтр по названию, описанию и пути, F5 для повторного поиска, Enter для вставки ссылки в промт |
| `/exit` или `/quit` | Выйти из приложения |
| `!<команда>` | Выполнить команду bash напрямую (например `!git status`, `!cargo test`) |
| `Ctrl+O` | Развернуть / свернуть вывод команд и полный текст ошибок |
| `Ctrl+C` | Скопировать выделенный текст поля; без выделения — прервать генерацию / команду, в пустой строке — выход |
| Двойной `Esc` | Очистить всё поле ввода (два нажатия в течение 500 мс) |
| `↑` / `↓` | Переместить курсор между строками поля; в пустом поле — история команд |
| `Alt+↑` / `Alt+↓` | История команд при заполненном поле |
| `Home` / `End` | Начало / конец текущей строки |
| `Ctrl+←` / `Ctrl+→` | Переместиться между словами |
| `Alt+Enter` | Добавить перенос строки; `Enter` отправляет запрос |
| `Shift` + стрелки, `Ctrl+A` | Выделить текст / всё поле |
| `Ctrl+X` | Вырезать выделенный текст поля |
| Клик ЛКМ в поле | Установить курсор в выбранную позицию |
| Выделение ЛКМ | Выделить текст поля или ответа; при отпускании кнопки он копируется в буфер обмена |
| ПКМ | Вставить содержимое буфера обмена в позицию курсора, без отправки запроса |
| `Ctrl+V` | Вставить текст, изображение или скопированные пути файлов из буфера обмена |

На Linux для буфера обмена используется `wl-clipboard` (Wayland), либо `xclip` / `xsel` (X11). Копирование, вставка и редактирование работают и во время ответа агента. Многострочный текст вставляется одним черновиком с настоящими переносами строк. Ввод или вставка заменяют выделенный текст.

Поле ввода показывает максимум три строки текста. Для просмотра длинного черновика прокручивайте колёсико над полем; над ответом колёсико прокручивает историю. При редактировании поле снова показывает позицию курсора.

Изображения PNG, JPEG, GIF и WebP можно вставлять из буфера обмена через `Ctrl+V` / ПКМ либо перетаскивать в терминал. В поле появляется `[Image: "..."]`; при отправке модель получает само изображение, а не только его путь. Такие вложения сохраняются в `.takiza/attachments` и в сессии, включая запросы в очереди. Остальные файлы вставляются только как пути, с сохранением пробелов в названиях; их содержимое автоматически не встраивается в запрос. Для просмотра изображений выбранная модель должна поддерживать их входной формат.

---

## Быстрый старт

1. Настройте ключи в файле `.env`:
   ```bash
   cp .env.example .env
   # Укажите ваш API ключ и модель
   ```

2. Запустите:
   ```bash
   cargo run --release
   ```

### Флаги запуска CLI

```bash
# Обычный запуск (по умолчанию запрашивает подтверждение перед вызовом run_command)
takiza

# Продолжить прошлую сессию (-c, --continue, --resume)
takiza -c

# Пропустить подтверждения команд, записи вне workspace и MCP / авто-одобрение (-y, --yes, -a, --auto-approve, --dangerously-skip-permissions)
takiza -y

# Комбинированные флаги: продолжить прошлую сессию и включить авто-одобрение
takiza -cy

# Запуск с начальным промптом
takiza -c "запусти тесты и исправь ошибки"
```

Или установите бинарник глобально:
```bash
cargo install --path .
takiza
```

---

## Релизы и публикация (GitHub Releases)

Сборка и публикация кроссплатформенных бинарников (Linux x86_64/musl/arm64, macOS Intel/Apple Silicon, Windows) автоматизирована через GitHub Actions и привязана к тегам:

```bash
# Создайте тег с версией и запушьте в репозиторий:
git tag -a v0.1.0 -m "Release v0.1.0"
git push origin v0.1.0
```

GitHub Actions автоматически соберет релизные архивы с контрольными суммами sha256 и создаст GitHub Release.

Скиллы обнаруживаются в `.takiza/skills`, `.agents/skills`, `.skills`, `.codex/skills` и `.claude/skills` внутри проекта. Глобальные каталоги: `~/.takiza/skills`, `~/.config/takiza/skills`, `~/.agents/skills`, `~/.codex/skills`, `~/.claude/skills`, а также каталоги Gemini и `$CODEX_HOME/skills`, если переменная задана. Каждый скилл содержит `SKILL.md`; вложенные каталоги поддерживаются. При совпадении названий локальный скилл имеет приоритет. Список автоматически обновляется в контексте модели перед следующим промтом; инструкции читаются через `read_skill` или `read_file`.

В `/skills` Enter вставляет `$имя-скилла` в поле ввода без отправки промта. Такую ссылку можно набрать вручную: `$frontend-design переделай страницу`. Также распознаются Markdown-ссылки на обнаруженный `SKILL.md`. При отправке нейронка получает явное указание прочитать и применить перечисленные скиллы к текущей задаче.

При вводе `$` в поле промта появляется встроенное меню скиллов, аналогичное меню `/` для команд. Продолжение имени фильтрует список; стрелки выбирают скилл, Enter или Tab вставляют ссылку без отправки промта, Esc закрывает подсказки. Меню работает и во время ответа модели, сохраняя видимую область чата.

Перед каждым запросом к модели сохраняется локальный снимок файлов проекта, привязанный к текущему чату. Лимита в 10 секунд нет: запрос начинается после завершения снимка; интерфейс остаётся доступным, Esc отменяет создание снимка и запрос. `/rewind` и `/restore` показывают только точки этого чата; в новом пустом чате точек нет. При `/resume` возвращаются точки выбранного чата. Команды TUI и прямые команды `!` не создают точки. Git не требуется. `/rewind` (алиас `/restore`) открывает список точек: стрелки выбирают точку, Enter показывает изменения, PgUp/PgDn листают список файлов. Подтверждение возвращает изменённые и удалённые файлы, удаляет новые файлы; перед откатом сохраняется резервная точка, через которую его можно отменить. Вместе с файлами восстанавливаются история интерфейса и контекст модели: запросы, ответы, рассуждения и результаты инструментов после выбранной точки удаляются из текущей сессии. Исходный запрос выбранной точки целиком возвращается в поле ввода: его можно отредактировать и отправить заново; автоматически он не отправляется. Резервная точка сохраняет и файлы, и чат, позволяя отменить откат. Откат чата доступен даже без изменений файлов. Для старых точек без снимка чата граница восстанавливается по уникальному запросу; неоднозначные точки не применяются. Активный запрос останавливается, очередь запросов очищается. Снимки доступны после перезапуска и хранятся в `.takiza/checkpoints` внутри рабочего каталога. Каталоги `.git`, `.takiza`, `node_modules`, `target`, `.venv`, `venv`, `__pycache__`, `.next` и `.cache` исключены; состояние Git (коммиты, ветки, staging) не меняется. Неизменившиеся файлы повторно используют данные предыдущего снимка, когда файловая система поддерживает hardlink.

## Версия сборки

Версия CLI (включая `takiza --version` и баннер) определяется при сборке по ближайшему
достижимому Git-тегу `vX.Y.Z` или `X.Y.Z`: например, `v0.1.1` → `0.1.1`.
Коммиты после тега сохраняют его версию. Ручное изменение `Cargo.toml` для релиза
не требуется. Если Git или подходящие теги недоступны (например, в архиве исходников),
используется версия из `Cargo.toml` с предупреждением сборки.

Release workflow получает полную историю и собирает выбранный тег, в том числе
при ручном запуске через `workflow_dispatch`.


### Web search

The agent has a `web_search` tool in its normal tool loop. It searches when asked
explicitly or when information is unfamiliar, uncertain or likely outdated.
Arguments: `query` (required) and `num_results` (optional, 1–10, default 5).
Results contain titles, source URLs and short snippets, not full page contents.
The model is instructed to cite source URLs and report failed verification honestly.

Search appears as ordinary command/tool execution, with compact output and the
usual expansion control. Stop cancels in-flight requests. Requests have a 20-second
outer deadline, a 15-second HTTP timeout and a 1 MiB response limit. Search uses
the configured proxy and never sends the model API key to a search provider.

Set `TAVILY_API_KEY` in the environment or loaded `.env` to use Tavily Search:

```dotenv
TAVILY_API_KEY=tvly-your-key
```

Create a key at https://app.tavily.com/ and restart Takiza after configuring it.
Tavily uses `search_depth=basic` and disables automatic parameter selection,
so each search consumes one API credit. Results include titles, URLs and snippets;
Takiza does not request generated answers, images or raw page contents.

Provider priority is Tavily, then `BRAVE_SEARCH_API_KEY` (Brave Search API), then
Bing RSS when neither key is configured. Failed authenticated searches are reported
without silently switching providers. Provider availability, limits and applicable usage terms
still apply; provider errors appear as ordinary tool failures.

При выходе через `/exit` или Ctrl+C в пустом поле ввода Takiza сохраняет чат и выводит статистику в обычной консоли: токены, число запросов и вызовов инструментов, время текущего запуска. Если провайдер не сообщил весь расход, выводится нижняя граница или отметка о недоступной статистике. Команда `takiza --session <id>` из сообщения продолжает именно этот чат в том же каталоге. Во время ответа Ctrl+C останавливает текущий запрос; Esc также отменяет выполнение.

Если модель исчерпала лимит одного ответа (`finish_reason: length`) на рассуждениях или вызове инструмента, Takiza один раз повторяет запрос с инструкцией сократить рассуждения и выполнять задачу небольшими шагами. Обрезанные вызовы инструментов не исполняются. При повторном исчерпании выводится причина остановки; размер окна контекста и лимит ответа — разные ограничения.

При явной ошибке переполнения контекста Takiza автоматически сжимает историю в краткую сводку и повторяет запрос. Если ошибка содержит точный максимальный размер окна, он сразу учитывается в индикаторе и сохраняется на семь дней в `context-limits` рядом с `config.json`. Кэш разделяется по endpoint, модели, учётным данным и прокси; API-ключ в файл не записывается. Если метаданные сообщают меньший лимит, применяется меньший. Ошибка без точного максимума и ручное сжатие не определяют лимит. `/compact` запускает сжатие вручную; Esc отменяет его. Во время сжатия видны процент обработанной истории и номер порции. Порции содержат до 128 000 символов, уменьшаются с учётом известного окна и при переполнении API. Один запрос сводки ограничен 45 секундами, всё сжатие — тремя минутами; тайм-аут не меняет исходную историю. Для сводки GLM-5.3-Flash на NVIDIA NIM используется `reasoning_effort=low` и до 8K токенов ответа, отдельно от настроек кодинга. Системные инструкции и последний промпт сохраняются целиком, видимая переписка остаётся в чате. При ошибке или отмене исходный контекст сохраняется.

Вызовы инструментов показываются компактными блоками: имя инструмента, основные аргументы и статус; ниже — короткий вывод. Отказ и остановка отличаются от ошибки выполнения. Ответы модели показываются полностью, а текстовые результаты команд — по три экранные строки. Ctrl+O раскрывает полный текст, аргументы и изменения файлов; повторное нажатие сворачивает. Полный текст сохраняется в истории и контексте модели.

### MCP and language servers

Takiza can expose MCP tools and read-only LSP analysis to the model. Add the
optional `mcpServers` and `lspServers` maps to your existing JSON configuration.
The existing selection stays the same: `.takiza/config.json` in the workspace
when that file exists, otherwise `~/.config/takiza/config.json`. Existing settings
and unrecognized configuration fields are preserved when saving preferences.

```json
{
  "mcpServers": {
    "local": {
      "transport": "stdio",
      "command": "your-mcp-server",
      "args": ["--stdio"],
      "env": {"EXAMPLE_SETTING": "value"}
    },
    "remote": {
      "transport": "http",
      "url": "https://example.com/mcp",
      "auth": {"type": "token", "env": "EXAMPLE_MCP_TOKEN"}
    },
    "higgsfield": {
      "transport": "http",
      "url": "https://mcp.higgsfield.ai/mcp",
      "auth": {"type": "oauth"}
    }
  },
  "lspServers": {
    "rust": {
      "command": "rust-analyzer",
      "args": [],
      "languages": ["rust"],
      "rootMarkers": ["Cargo.toml", "rust-project.json"]
    },
    "typescript": {
      "command": "typescript-language-server",
      "args": ["--stdio"],
      "languages": ["typescript", "typescriptreact", "javascript", "javascriptreact"],
      "rootMarkers": ["tsconfig.json", "jsconfig.json", "package.json"]
    },
    "python": {
      "command": "pyright-langserver",
      "args": ["--stdio"],
      "languages": ["python"],
      "rootMarkers": ["pyproject.toml", "pyrightconfig.json", "setup.py"]
    }
  }
}
```

This is a fragment to merge into the existing configuration. Restart Takiza after
editing server configuration. MCP discovery starts before the first model request;
LSP starts on the first request for a matching language. Install server programs
explicitly yourself; Takiza does not install them. Server environment maps are
optional. Keep credentials in environment variables rather than JSON.

- `/tools` shows the current registry with scrolling for long lists.
- `/mcp status`, `/mcp connect NAME`, `/mcp disconnect NAME`, `/mcp restart NAME`.
- `/mcp login NAME` opens OAuth in a browser and displays a fallback URL. The
  loopback callback expires after three minutes. Complete login, then use
  `/mcp connect NAME` or send the next prompt. `/mcp logout NAME` removes the
  system credential and disconnects the server. A provider without dynamic
  registration needs a registered `clientId` in its OAuth configuration.
- `/lsp status`, `/lsp connect NAME`, `/lsp disconnect NAME`, `/lsp restart NAME`.

OAuth uses discovery, PKCE and state validation. Tokens and refreshed credentials
are stored only in the system credential store, with session memory as the
fallback when that store is unavailable. Browser login is asynchronous; the TUI
remains usable. The authorization URL is visible in terminal history but OAuth
tokens are excluded from tool output and application logs.

Before an MCP call, review its server, original tool name and complete arguments.
Choose allow once, deny, or trust that server for the current run. `-y` approves
all calls. Server trust survives chat reset and model changes but ends on
explicit disconnect/logout or application exit. Failed calls are not retried;
a disconnected or expired HTTP session requires explicit reconnection. MCP
names use `mcp__SERVER__TOOL`, encode unusual bytes and receive a stable suffix
when they exceed the provider's 64-character limit. Results preserve text,
structured data and resource links; supported images use Takiza attachments.
Unsupported content formats are labeled explicitly.

File edits within the workspace run without confirmation. `write_file` and
`edit_file` destinations outside the workspace require approval with the
requested path, resolved path, and proposed diff. Existing symlinks and `..`
are resolved before checking the boundary, including for new files. Allow once
or deny; approving a file write does not enable global auto-approval. Changes
to the destination or file contents during review invalidate that approval.
`-y` or enabled `auto_approve` skips this confirmation too. This is a file-tool
approval boundary, not an operating-system sandbox for shell or MCP tools.

The Higgsfield URL comes from its [official integration guide](https://higgsfield.ai/creator-hub/help-center/integrations/how-do-i-connect-higgsfield-to-ai-agent).
Live login requires your account. Request generation explicitly before approving
any generation tool; connecting a server only discovers tools.

LSP tools are `lsp_hover`, `lsp_definition`, `lsp_references`, `lsp_symbols`, and
`lsp_diagnostics`. The system prompt guides the agent to use available LSP tools
for semantic code analysis and diagnostics after relevant edits, and connected
MCP tools when their capabilities fit the task. Explicit tool names in your
request are optional; the current registry supplies names and argument schemas.
Builds and tests still provide separate validation.

File tools take a workspace-relative `path`. Hover, definition
and references also take `line` and `column`: both start at 1, and columns count
Unicode characters, not UTF-8 bytes or UTF-16 units. Responses convert positions
back to this convention and file URIs to workspace paths. `lsp_symbols` accepts
`path` for file symbols or `query` plus `server` for project symbol search.
Capabilities are checked before sending requests. Documents are synchronized
from disk before requests, including shell edits; `/rewind` stops LSP processes
and clears documents so restored files reopen with fresh versions.

Diagnostics return `current`, `pending`, or `unverified`. An empty `current` list
means the server checked that version and reported no diagnostics. A push server
that omits versions returns `unverified`; those results do not confirm that the
current file is clean. Pull diagnostics, when supported, check current content.
Server failures are returned as unavailable errors and do not disable builtins.
LSP requests time out after 15 seconds, MCP calls after at most 60 seconds;
cancellation is forwarded and processes shut down on exit. Server management
commands can be cancelled with Escape or Ctrl+C and time out after 35 seconds.

For a reproducible real-server check, install compatible servers and run:

```sh
TAKIZA_TEST_LSP_BIN_DIR=/path/to/node_modules/.bin \
TAKIZA_TEST_RUST_ANALYZER=/path/to/rust-analyzer \
cargo test installed_rust_typescript_javascript_python_servers -- --ignored --nocapture
```

The test uses isolated temporary projects and does not install servers. TS/JS
projects need a compatible TypeScript package; verification used TypeScript 5.9.3.

The initial scope is MCP tools over stdio or Streamable HTTP and read-only LSP.
MCP resources/prompts discovery, legacy HTTP+SSE, server installation and LSP
rename/format/code actions are not exposed.

Protocol fixtures run in `cargo test` and require Python 3. They cover stdio,
Streamable HTTP, pagination, OAuth/PKCE/refresh, disconnection, Unicode positions,
diagnostics and disk edits. PTY checks live under `tests/tui_*_pty.py`.
