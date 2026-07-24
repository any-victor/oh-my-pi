<conversation>
{{conversation}}
</conversation>
{{#if previousSummary}}

<previous-summary>
{{previousSummary}}
</previous-summary>
{{/if}}
{{#if additionalContext}}

<additional-context>
{{#each additionalContext}}
- {{this}}
{{/each}}
</additional-context>
{{/if}}

You MUST summarize what was done in this conversation, written like a pull request description.

Rules:
- MUST be 2-3 sentences max
- MUST describe the changes made, not the process
- NEVER mention running tests, builds, or other validation steps
- NEVER explain what the user asked for
- MUST write in first person (I added…, I fixed…)
- NEVER ask questions
