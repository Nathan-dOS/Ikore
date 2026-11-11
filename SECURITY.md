# Security policy

## Reporting a vulnerability

Do not report vulnerabilities in public issues, pull requests or discussions.

Report them privately through GitHub: open the repository's **Security** tab
and choose **Report a vulnerability**. GitHub private vulnerability reporting
shows the report only to you and the maintainers.

Include what you can:

- the affected crate, file or feature and the Ikore commit;
- the steps to reproduce and the impact you observed;
- your environment: Windows version, CLI backend or provider, and whether you
  used Docker.

Never include real credentials, tokens, API keys, personal paths or private
prompt content. Replace them with obviously synthetic values
(`fixture-secret-value`, `example.test`) and say where the real value would
appear. If you believe a real credential was exposed, say so in the report
without the value.

## Scope and versions

Binary releases are not distributed (see `quality/distribution-policy.json`).
Fixes land on `main`; build from the latest `main` to check whether an issue
is still present.
