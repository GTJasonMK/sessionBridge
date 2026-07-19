# Security Policy

## Supported Versions

Until SessionBridge reaches a stable release, security fixes are applied to
the latest release and the current default branch only. Older development
snapshots are not maintained.

## Reporting a Vulnerability

Use GitHub's private vulnerability reporting for this repository:

1. Open the repository's **Security** tab.
2. Select **Advisories**.
3. Select **Report a vulnerability**.

Do not open a public issue containing a vulnerability, invitation token,
Session token, context file, database, private prompt, filesystem layout, or
real collaboration log. If private vulnerability reporting is unavailable,
contact a repository maintainer through a private channel listed on their
GitHub profile and include only enough information to establish a secure
follow-up channel.

Include:

- affected version or commit;
- operating system and filesystem type;
- attack prerequisites and trust-boundary assumptions;
- minimal reproduction steps;
- expected and observed behavior;
- impact on secrets, state integrity, Git worktrees, or command execution;
- any proposed mitigation.

The maintainers will acknowledge a complete report within five business days,
coordinate validation and remediation privately, and publish an advisory after
a fix or mitigation is available.

## Security Scope

SessionBridge assumes collaborating processes run under one trusted OS user.
It protects against accidental disclosure and coordination corruption, but it
is not a sandbox or an isolation boundary between mutually hostile processes
owned by that user.
