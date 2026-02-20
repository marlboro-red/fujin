# Fujin Pipeline Guides

Focused guides for writing effective pipelines with fujin.

## Guides

### [Getting Started](getting-started.md)
Create your first pipeline from scratch. Walks through single-stage, multi-stage, and command stages with a realistic Python CLI project.

### [Multi-Stage Pipelines](multi-stage-pipelines.md)
Design effective stage sequences. Covers context passing (`{{prior_summary}}`, artifacts, named stage references), model selection, tool restrictions, max_turns tuning, and mixing command stages with agent stages.

### [Branching and Conditions](branching-and-conditions.md)
Make stages conditional. Covers `when` (regex gating), `branch`/`on_branch` (AI-driven routing), convergence points, and combining both mechanisms.

### [Exports and Dynamic Variables](exports-and-dynamic-variables.md)
Let agents discover information at runtime and pass it to downstream stages. Covers the exports feature, prompt patterns, fallback defaults, and adaptive pipeline design.

### [Pipeline Patterns](pipeline-patterns.md)
Copy-pasteable pipeline recipes for common workflows: implement-test-fix loops, code review, spec-driven development, adaptive analysis, conditional testing, multi-model brainstorming, PR preparation, migrations, refactoring, and documentation generation.

## Reference

### [Pipeline Authoring Reference](../pipeline-authoring.md)
Complete field reference for all pipeline config options, template variables, validation rules, and runtime details.
