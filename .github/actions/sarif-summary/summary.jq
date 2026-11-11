# Markdown table of the findings in a SARIF report (see action.yaml).
# $title: the heading. $blob: where file links point (…/blob/<commit>).
# $root: the checkout path, taken off absolute file paths.

# Text that is safe inside a markdown table cell.
def cell: tostring | gsub("[\r\n]+"; " ") | gsub("\\|"; "\\|") | gsub("<"; "&lt;") | gsub(">"; "&gt;");

def levels: ["error", "warning", "note", "none"];
def icon: {error: "❌", warning: "⚠️", note: "ℹ️"}[.] // "";

# The table stops here (most severe first); the job's log has them all.
def max_rows: 50;

# Rules without a link in the report: hadolint's own (DL…) and ShellCheck's (SC…).
def help($rules):
  . as $id
  | ([$rules[] | select(.id == $id) | .helpUri // empty] | first)
    // if test("^DL[0-9]+$") then "https://github.com/hadolint/hadolint/wiki/\(.)"
       elif test("^SC[0-9]+$") then "https://www.shellcheck.net/wiki/\(.)"
       else null end;

[ .runs[] | (.tool.driver.rules // []) as $rules | .results[]?
  | select((.suppressions // []) | length == 0)
  | (.locations[0].physicalLocation // {}) as $at
  | {
      level: (.level // "warning"),
      rule: (.ruleId // "?"),
      help: ((.ruleId // "") | help($rules)),
      file: (($at.artifactLocation.uri // "") | sub("^file://"; "") | ltrimstr($root)),
      line: $at.region.startLine,
      message: (.message.text // "")
    }
] as $all
| ($all | sort_by([(.level as $l | levels | index($l) // 4), .file, .line // 0])) as $sorted
| "## \($title)",
  "",
  if ($all | length) == 0 then "✅ No findings."
  else
    "\($all | length) finding\(if ($all | length) == 1 then "" else "s" end):",
    "",
    "| Level | Rule | Where | Message |",
    "| --- | --- | --- | --- |",
    ($sorted[:max_rows][]
      | (if .help then "[\(.rule | cell)](\(.help))" else (.rule | cell) end) as $rule
      | (if .file == "" then "" elif .line then "[\(.file | cell):\(.line)](\($blob)/\(.file)#L\(.line))" else "[\(.file | cell)](\($blob)/\(.file))" end) as $where
      # No code spans: they'd show the escaped < and > as &lt; and &gt;.
      | "| \(.level | icon) \(.level) | \($rule) | \($where) | \(.message | gsub("`"; "") | cell) |"),
    (if ($all | length) > max_rows then "", "…and \(($all | length) - max_rows) more: see the job's log." else empty end)
  end
