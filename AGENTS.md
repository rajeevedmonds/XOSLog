# Git Author Rule (mandatory)

For **every** Git commit in **every** project, use exactly this author and
email. Do not use any other author, co-author, or committer identity for this
project or any future project.

- Author name: `Rajeev Edmonds`
- Author email: `contact@rajeevedmonds.com`

This is also set as the global Git config (`git config --global user.name` /
`user.email`).

## Enforcement

- Commit with: `git commit --author="Rajeev Edmonds <contact@rajeevedmonds.com>"`
  (or rely on the global config).
- Remove/never create `Co-authored-by` trailers and do not use the platform's
  auto-injected author identities.
- If a `prepare-commit-msg` hook or `coauthor.*` config would add another
  identity, disable/remove it before committing.
- Before committing, verify with `git log --format="%an <%ae>" -1` that the
  author is `Rajeev Edmonds <contact@rajeevedmonds.com>`.
