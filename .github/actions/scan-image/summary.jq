# Markdown summary of a Trivy JSON report, for the job summary (see action.yml).
# Builds fail on HIGH or CRITICAL vulnerabilities that have a fix.
# $url: the findings in Security → Code scanning.

# Text that is safe inside a markdown table cell.
def cell: tostring | gsub("[\r\n]+"; " ") | gsub("\\|"; "\\|") | gsub("<"; "&lt;") | gsub(">"; "&gt;");

[.Results[]?.Vulnerabilities[]?] as $v
| [$v[] | select((.Severity == "HIGH" or .Severity == "CRITICAL") and (.FixedVersion // "") != "")] as $bad
| "## Image scan (Trivy)",
  "",
  "\(.Metadata.OS.Family // "?") \(.Metadata.OS.Name // "") base image. Builds fail on HIGH or CRITICAL vulnerabilities that have a fix.",
  "",
  "| Severity | With a fix | All |",
  "| --- | --: | --: |",
  (["CRITICAL", "HIGH", "MEDIUM", "LOW", "UNKNOWN"][] as $s
    | "| \($s) | \([$v[] | select(.Severity == $s and (.FixedVersion // "") != "")] | length) | \([$v[] | select(.Severity == $s)] | length) |"),
  "",
  if ($bad | length) == 0 then "✅ Nothing that fails the build."
  else "❌ These fail the build:", "",
    "| Package | Vulnerability | Installed | Fixed in |",
    "| --- | --- | --- | --- |",
    ($bad[] | "| \(.PkgName | cell) | [\(.VulnerabilityID | cell)](\(.PrimaryURL // "")) | \(.InstalledVersion | cell) | \(.FixedVersion | cell) |")
  end,
  if ($v | length) == 0 then empty
  else "", "Details of each finding: [Security → Code scanning](\($url))"
  end
