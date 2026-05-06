//! Ember.js / Glimmer / Embroider plugin.
//!
//! Activates on `ember-source`, `ember-cli`, `@embroider/core`,
//! `@embroider/compat`, or `@glimmer/component` dependencies. Tracks Ember's
//! build, test, and runtime tooling deps (so they are not flagged as unused),
//! whitelists the lifecycle and reflectively-invoked members on Ember's class
//! hierarchy, exposes Ember's filesystem-resolved conventions (the classic
//! `app/`, `addon/`, and `tests/` layouts) as entry-point globs since those
//! files are loaded by the Ember resolver rather than by static `import`,
//! and declares Ember's `@ember/*` namespace as a virtual module prefix so
//! Embroider-rewritten specifiers like `@ember/object` and
//! `@ember/routing/router-service` don't surface as `unresolved-import`.
//!
//! Template-block import tracking (`<template>...</template>`, `.gjs`/`.gts`
//! single-file components, and `.hbs` references) is handled separately by the
//! Glimmer-aware extractor in `crates/extract/src/glimmer.rs`. **Co-located
//! `.hbs` templates remain a known limitation** of that extractor: imports
//! consumed only by a sibling `.hbs` file still surface as `unused-import` on
//! the sibling `.js`/`.ts` — see the module-level note in
//! `crates/extract/src/sfc_template/glimmer.rs`. `ENTRY_PATTERNS` below
//! includes `*.hbs` paths so the templates themselves stay reachable as
//! files; binding-level usage tracking inside them is out of scope until the
//! scanner gains a Handlebars front-end. Migrating a component to `.gts`
//! removes the limitation entirely. Decorator-form
//! component, service, helper, and modifier registration (`@classic`,
//! `@service`, `@tracked`, `@action`) flows through the visitor and is not
//! re-implemented here. This plugin only handles the lifecycle and convention
//! members that the framework calls reflectively at runtime.

use fallow_config::{ScopedUsedClassMemberRule, UsedClassMemberRule};

use super::Plugin;

const ENABLERS: &[&str] = &[
    "ember-source",
    "ember-cli",
    "@embroider/core",
    "@embroider/compat",
    "@glimmer/component",
];

/// Packages required by an Ember project but never statically imported from
/// source. Anything an `import` statement can reach in a modern Ember app
/// (`@glimmer/component`, `@ember/test-helpers`, `@ember-data/*`, ...) is
/// intentionally omitted: the normal import graph already credits those, so
/// listing them here would only mask real removals when a user genuinely
/// drops a dependency.
///
/// Note: a package may legitimately appear in BOTH this list and `ENABLERS`.
/// `ember-source` is both the activation signal (its presence in
/// `package.json` is how we detect an Ember project) AND a runtime-resolved
/// dependency that no source file imports directly. The two roles are
/// independent: enablers gate plugin activation; tooling deps suppress
/// `unused-dependency` for build-/CLI-/runtime-resolved packages. Don't
/// dedupe.
const TOOLING_DEPENDENCIES: &[&str] = &[
    // Core Ember runtime / build pipeline
    //
    // `ember-source` is the meta-package: source code imports through the
    // `@ember/*` namespace (e.g. `@ember/application`, `@ember/routing/route`)
    // and never references the `ember-source` specifier directly.
    "ember-source",
    "ember-cli",
    "ember-cli-htmlbars",
    "ember-cli-babel",
    "ember-auto-import",
    // Embroider runtime core. The compat / webpack / vite halves are
    // `require()`'d from `ember-cli-build.js` (which is an entry pattern),
    // and `@embroider/addon-shim` is `require()`'d from each v2 addon's
    // `index.js` (reached via `package.json#main`); they're credited through
    // the normal import graph and don't need an allowlist entry. The macros /
    // router / test-setup halves are imported from source and likewise rely
    // on the import graph.
    "@embroider/core",
    // Glint type-checker CLI + tsconfig environment shims (`@glint/template`
    // IS imported as type-only and so is omitted here).
    "@glint/core",
    "@glint/environment-ember-loose",
    "@glint/environment-ember-template-imports",
    // Test infrastructure invoked by the runner, not imported from source
    // (`ember-qunit`, `qunit`, `qunit-dom`, `@ember/test-helpers` are imported
    // and so are omitted here).
    "ember-cli-test-loader",
    "ember-exam",
    // Common addons that act through ember-cli config, package.json keys, or
    // the build server rather than via source imports.
    "ember-template-lint",
    "ember-template-imports",
    "ember-source-channel-url",
    "@ember/optional-features",
    "ember-cli-dependency-checker",
    "ember-cli-inject-live-reload",
    "ember-cli-sri",
    "ember-cli-terser",
    "loader.js",
];

/// Glimmer / classic Ember component lifecycle members called by the framework
/// at runtime. Covers both `@glimmer/component` and the legacy
/// `@ember/component` class hierarchy.
const COMPONENT_MEMBERS: &[&str] = &[
    "willDestroy",
    "didInsertElement",
    "didRender",
    "didUpdate",
    "didReceiveAttrs",
    "willRender",
    "willUpdate",
    "willClearRender",
    "willDestroyElement",
    "didDestroyElement",
];

/// Route hooks called by the Ember router during transitions, plus the
/// convention properties (`actions`, `queryParams`, `templateName`,
/// `controllerName`) that the resolver reads reflectively.
const ROUTE_MEMBERS: &[&str] = &[
    "model",
    "beforeModel",
    "afterModel",
    "setupController",
    "resetController",
    "redirect",
    "serialize",
    "deserialize",
    "activate",
    "deactivate",
    "actions",
    "queryParams",
    "templateName",
    "controllerName",
];

/// Controller convention properties the Ember resolver reads reflectively.
/// `actions` and `queryParams` are the common cases; `templateName` and
/// `controllerName` are documented Ember APIs that the route's resolver
/// honors when looking up the paired template / controller for a route, so
/// declaring them on a Controller subclass must not be flagged unused.
const CONTROLLER_MEMBERS: &[&str] = &["actions", "queryParams", "templateName", "controllerName"];

const SERVICE_MEMBERS: &[&str] = &["init", "willDestroy"];

const HELPER_MEMBERS: &[&str] = &["compute", "recompute"];

const MODIFIER_MEMBERS: &[&str] = &[
    "modify",
    "willDestroy",
    "didReceiveArguments",
    "didInstall",
    "didUpdateArguments",
    "willRemove",
];

const APPLICATION_MEMBERS: &[&str] = &["ready", "willDestroy", "init"];

const ROUTER_MEMBERS: &[&str] = &[
    "map",
    "location",
    "rootURL",
    "willTransition",
    "didTransition",
];

/// Import-specifier prefixes that `ember-source` exposes through the AMD
/// loader (classic) or the Embroider rewriter (Embroider) rather than as
/// separate npm packages. Anything matching one of these prefixes is
/// suppressed from `unresolved-import` and `unlisted-dependency` reporting.
///
/// The list is **enumerated, not a blanket `@ember/`**, because parts of the
/// `@ember/*` namespace ARE real npm packages users install explicitly and
/// must keep listed in `package.json`:
///
/// - `@ember/test-helpers`
/// - `@ember/render-modifiers`
/// - `@ember/test-waiters`
/// - `@ember/string`
/// - `@ember/jquery`
/// - `@ember/legacy-built-in-components`
/// - `@ember/optional-features`
///
/// Silencing those with a blanket prefix would mask real missing-dep bugs.
///
/// Source of truth for the virtual list: `ember-source`'s `package.json`
/// `exports` field. Keep this in sync (it changes slowly — most additions
/// land as new subpaths under existing roots like `@ember/object/...` which
/// the prefix-match already covers).
///
/// Known gaps NOT covered (documented; users can `ignoreDependencies` or
/// add an inline `fallow-ignore-next-line unresolved-import`):
///
/// - Bare `import Ember from 'ember'` — a legacy Embroider-rewritten
///   specifier. A `"ember"` prefix would also catch `ember-cli`,
///   `ember-data`, `ember-source` and silence legitimate missing-dep
///   reports, so we leave it.
/// - v1 Ember addon subpaths (`ember-in-viewport/modifiers/in-viewport`,
///   `ember-power-select/components/...`): the v1 addon `addon/` tree
///   convention is not Node `exports`. A proper fix is addon-shape probing
///   in the resolver; the escape hatch today is `ignoreDependencies` per
///   addon (or migrating to its v2 build).
const VIRTUAL_MODULE_PREFIXES: &[&str] = &[
    // Bare `ember` (legacy `import Ember from 'ember'`). The trailing slash
    // is load-bearing: it makes the entry exact-match `ember` (via the
    // `strip_suffix('/')` shortcut in the suppression logic) AND match any
    // future `ember/<subpath>` shape without also covering `ember-cli`,
    // `ember-data`, or `ember-source`. A no-slash entry would prefix-match
    // every `ember-*` real npm package and mask legitimate missing-dep
    // reports.
    "ember/",
    // ember-source modules exposed via the AMD loader / Embroider rewriter.
    // Each entry covers its bare specifier plus every subpath
    // (`@ember/object` also covers `@ember/object/computed`,
    // `@ember/object/proxy`, etc.) via `starts_with`.
    "@ember/application",
    "@ember/array",
    "@ember/canary-features",
    "@ember/component",
    "@ember/controller",
    "@ember/debug",
    "@ember/destroyable",
    "@ember/engine",
    "@ember/enumerable",
    "@ember/error",
    "@ember/helper",
    "@ember/instrumentation",
    "@ember/modifier",
    "@ember/object",
    "@ember/owner",
    "@ember/renderer",
    "@ember/routing",
    "@ember/runloop",
    "@ember/service",
    // `@ember/template` covers `template-compilation`, `template-compiler`,
    // and `template-factory` via prefix-match.
    "@ember/template",
    "@ember/utils",
    "@ember/version",
];

/// Substring markers for build-time template-placeholder imports that the
/// Ember build pipeline substitutes before the JS reaches a bundler. They
/// leak into `unresolved-import` because fallow's HTML asset scanner sees
/// raw specifiers like `./{{rootURL}}assets/cio.js` in `app/index.html` and
/// `###APPNAME###/...` in ember-cli blueprint files. Both are unanchored
/// (matched as `spec.contains(substring)`):
///
/// - `{{` covers any Handlebars expression in a specifier
///   (`{{rootURL}}`, `{{config.assetsPath}}`, ...) — Handlebars is Ember's
///   templating language and `{{...}}` never appears in a real import
///   specifier.
/// - `###` covers ember-cli blueprint placeholders (`###APPNAME###`,
///   `###DUMMY###`, ...) — these are scaffolded files that haven't been
///   processed yet, or addon-fixture blueprints checked into source.
///
/// Both markers are safe across the Ember ecosystem and don't collide with
/// any legitimate import specifier shape.
const PLACEHOLDER_SUBSTRINGS: &[&str] = &["{{", "###"];

const ENTRY_PATTERNS: &[&str] = &[
    // Classic app/ layout
    "app/app.{js,ts,gjs,gts}",
    "app/router.{js,ts}",
    "app/index.html",
    "app/components/**/*.{js,ts,gjs,gts,hbs}",
    "app/routes/**/*.{js,ts,gjs,gts}",
    "app/controllers/**/*.{js,ts}",
    "app/templates/**/*.{hbs,gjs,gts}",
    "app/models/**/*.{js,ts}",
    "app/services/**/*.{js,ts}",
    "app/helpers/**/*.{js,ts,gjs,gts}",
    "app/modifiers/**/*.{js,ts}",
    "app/initializers/**/*.{js,ts}",
    "app/instance-initializers/**/*.{js,ts}",
    "app/adapters/**/*.{js,ts}",
    "app/serializers/**/*.{js,ts}",
    "app/transforms/**/*.{js,ts}",
    // v1 addon layout
    "addon/**/*.{js,ts,gjs,gts,hbs}",
    "addon-test-support/**/*.{js,ts,gjs,gts}",
    // Tests
    "tests/test-helper.{js,ts}",
    "tests/index.html",
    "tests/**/*-test.{js,ts,gjs,gts}",
    // Build / config
    "config/environment.js",
    "config/targets.js",
    "config/optional-features.json",
    "config/deprecation-workflow.js",
    "ember-cli-build.js",
    "testem.js",
];

fn scoped_rule(extends: &str, members: &[&str]) -> UsedClassMemberRule {
    UsedClassMemberRule::Scoped(ScopedUsedClassMemberRule {
        extends: Some(extends.to_string()),
        implements: None,
        members: members.iter().map(|s| (*s).to_string()).collect(),
    })
}

pub struct EmberPlugin;

impl Plugin for EmberPlugin {
    fn name(&self) -> &'static str {
        "ember"
    }

    fn enablers(&self) -> &'static [&'static str] {
        ENABLERS
    }

    fn tooling_dependencies(&self) -> &'static [&'static str] {
        TOOLING_DEPENDENCIES
    }

    fn used_class_member_rules(&self) -> Vec<UsedClassMemberRule> {
        vec![
            scoped_rule("Component", COMPONENT_MEMBERS),
            scoped_rule("Route", ROUTE_MEMBERS),
            scoped_rule("Controller", CONTROLLER_MEMBERS),
            scoped_rule("Service", SERVICE_MEMBERS),
            scoped_rule("Helper", HELPER_MEMBERS),
            scoped_rule("Modifier", MODIFIER_MEMBERS),
            scoped_rule("Application", APPLICATION_MEMBERS),
            scoped_rule("Router", ROUTER_MEMBERS),
        ]
    }

    fn virtual_module_prefixes(&self) -> &'static [&'static str] {
        VIRTUAL_MODULE_PREFIXES
    }

    fn generated_import_substrings(&self) -> &'static [&'static str] {
        PLACEHOLDER_SUBSTRINGS
    }

    fn entry_patterns(&self) -> &'static [&'static str] {
        ENTRY_PATTERNS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enablers_cover_classic_embroider_and_glimmer() {
        let plugin = EmberPlugin;
        assert!(plugin.enablers().contains(&"ember-source"));
        assert!(plugin.enablers().contains(&"@embroider/core"));
        assert!(plugin.enablers().contains(&"@glimmer/component"));
    }

    #[test]
    fn tooling_dependencies_cover_runtime_only_packages() {
        let plugin = EmberPlugin;
        let deps = plugin.tooling_dependencies();
        // Build / CLI / config-only packages that no source file imports must
        // be credited via the tooling list.
        assert!(deps.contains(&"ember-source"));
        assert!(deps.contains(&"ember-cli-htmlbars"));
        assert!(deps.contains(&"@embroider/core"));
        assert!(deps.contains(&"@glint/core"));
        assert!(deps.contains(&"ember-exam"));
        assert!(deps.contains(&"loader.js"));
    }

    #[test]
    fn tooling_dependencies_omits_source_imported_packages() {
        // Packages a modern Ember app imports directly (`import Component from
        // '@glimmer/component'`, `import { tracked } from '@glimmer/tracking'`,
        // `import { module, test } from 'qunit'`, etc.) MUST NOT appear in
        // the tooling list. The normal import graph already credits them, and
        // listing them here would mask a real removal when a user genuinely
        // drops the dependency.
        let plugin = EmberPlugin;
        let deps = plugin.tooling_dependencies();
        for name in [
            "@glimmer/component",
            "@glimmer/tracking",
            "@glimmer/env",
            "@glint/template",
            "@ember/test-helpers",
            "ember-qunit",
            "qunit",
            "qunit-dom",
            "ember-data",
            "@ember-data/store",
            "@ember-data/model",
            "@embroider/macros",
            "@embroider/router",
            "@embroider/test-setup",
            // Reached via the normal import graph through `ember-cli-build.js`
            // (an entry pattern) which `require()`s the build half, and via
            // each v2 addon's `package.json#main` index.js for the shim.
            "@embroider/webpack",
            "@embroider/vite",
            "@embroider/addon-shim",
            "ember-load-initializers",
            "ember-resolver",
        ] {
            assert!(
                !deps.contains(&name),
                "{name} is imported from source in modern Ember; remove from tooling_dependencies"
            );
        }
    }

    #[test]
    fn lifecycle_rules_scope_component_members_to_glimmer_component() {
        let rules = EmberPlugin.used_class_member_rules();
        let component_rule = rules.iter().find_map(|r| match r {
            UsedClassMemberRule::Scoped(s) if s.extends.as_deref() == Some("Component") => Some(s),
            _ => None,
        });
        let component_rule = component_rule.expect("Component-scoped rule missing");
        assert!(component_rule.members.iter().any(|m| m == "willDestroy"));
        assert!(
            component_rule
                .members
                .iter()
                .any(|m| m == "didInsertElement")
        );
    }

    #[test]
    fn lifecycle_rules_scope_route_members_to_route_class() {
        let rules = EmberPlugin.used_class_member_rules();
        let route_rule = rules.iter().find_map(|r| match r {
            UsedClassMemberRule::Scoped(s) if s.extends.as_deref() == Some("Route") => Some(s),
            _ => None,
        });
        let route_rule = route_rule.expect("Route-scoped rule missing");
        assert!(route_rule.members.iter().any(|m| m == "model"));
        assert!(route_rule.members.iter().any(|m| m == "beforeModel"));
        assert!(route_rule.members.iter().any(|m| m == "setupController"));
    }

    #[test]
    fn unrelated_classes_get_no_lifecycle_rule_match() {
        let rules = EmberPlugin.used_class_member_rules();
        for r in &rules {
            let UsedClassMemberRule::Scoped(s) = r else {
                continue;
            };
            assert!(!s.matches_heritage(Some("UserService"), &[]));
        }
    }

    #[test]
    fn entry_patterns_cover_classic_layout() {
        let plugin = EmberPlugin;
        let patterns = plugin.entry_patterns();
        assert!(patterns.contains(&"app/components/**/*.{js,ts,gjs,gts,hbs}"));
        assert!(patterns.contains(&"tests/**/*-test.{js,ts,gjs,gts}"));
    }

    /// Check that an import specifier `spec` would be silenced by the
    /// plugin's virtual-module prefix list. Delegates to the production
    /// `matches_virtual_prefix` matcher so this test cannot drift from the
    /// `unresolved-import` / `unlisted-dependency` suppression sites in
    /// `crates/core/src/analyze/unused_deps.rs`.
    fn is_covered(prefixes: &[&str], spec: &str) -> bool {
        prefixes
            .iter()
            .any(|prefix| crate::analyze::unused_deps::matches_virtual_prefix(prefix, spec))
    }

    #[test]
    fn virtual_module_prefixes_cover_ember_source_runtime() {
        // Every specifier the user actually encounters in a strict-mode
        // Ember app and that `ember-source` rewrites at build time must be
        // covered. Includes top-level paths, subpaths under those roots,
        // the `template-*` family covered by the bare `@ember/template`
        // prefix via starts_with, and the bare `ember` legacy specifier
        // covered exactly via the `ember/` trailing-slash entry.
        let prefixes = EmberPlugin.virtual_module_prefixes();
        for spec in [
            // The exact specifiers from the original bug report.
            "@ember/object",
            "@ember/object/computed",
            "@ember/template",
            "@ember/service",
            "@ember/runloop",
            "@ember/utils",
            "@ember/routing/router-service",
            "@ember/helper",
            "@ember/modifier",
            // Other common runtime entries.
            "@ember/application",
            "@ember/component",
            "@ember/component/helper",
            "@ember/controller",
            "@ember/debug",
            "@ember/destroyable",
            "@ember/object/proxy",
            "@ember/routing/route",
            "@ember/template-compilation",
            "@ember/template-factory",
            "@ember/owner",
            // Bare `ember` (legacy `import Ember from 'ember'`).
            "ember",
        ] {
            assert!(
                is_covered(prefixes, spec),
                "expected `{spec}` to be silenced by the virtual-module \
                 prefix list (it is rewritten by Embroider / the AMD loader \
                 and not resolvable through node_modules); prefixes = \
                 {prefixes:?}",
            );
        }
    }

    #[test]
    fn virtual_module_prefixes_do_not_swallow_real_ember_npm_packages() {
        // Parts of the `@ember/*` namespace ARE real npm packages users
        // install explicitly. Silencing them with a blanket prefix would
        // mask legitimate `unlisted-dependency` reports when a user removes
        // one from `package.json`. This test is the regression fence
        // against re-introducing a blanket `@ember/` prefix.
        let prefixes = EmberPlugin.virtual_module_prefixes();
        for real in [
            "@ember/test-helpers",
            "@ember/render-modifiers",
            "@ember/test-waiters",
            "@ember/string",
            "@ember/jquery",
            "@ember/legacy-built-in-components",
            "@ember/optional-features",
        ] {
            assert!(
                !is_covered(prefixes, real),
                "`{real}` is a real npm package and must NOT be covered by \
                 the virtual-module prefix list; prefixes = {prefixes:?}",
            );
        }
    }

    #[test]
    fn virtual_module_prefixes_do_not_swallow_ember_dash_packages() {
        // `@ember-data/*`, `@glimmer/*`, and the entire `ember-*` family of
        // real npm packages (`ember-source`, `ember-cli`, `ember-data`,
        // `ember-in-viewport`, ...) resolve through normal node resolution.
        // A missing or misspelled specifier in any of those namespaces must
        // still surface. The `ember/` virtual entry uses a trailing slash
        // precisely so it ONLY matches bare `ember` and `ember/<subpath>`,
        // not `ember-<anything>`.
        let prefixes = EmberPlugin.virtual_module_prefixes();
        for spec in [
            "@ember-data/store",
            "@ember-data/model",
            "@glimmer/component",
            "@glimmer/tracking",
            "ember-source",
            "ember-cli",
            "ember-data",
            "ember-in-viewport",
            "ember-template-lint",
        ] {
            assert!(
                !is_covered(prefixes, spec),
                "`{spec}` must NOT be covered by the virtual-module prefix \
                 list; prefixes = {prefixes:?}",
            );
        }
    }

    #[test]
    fn generated_import_substrings_cover_ember_template_placeholders() {
        // The two markers must match the placeholder specifiers fallow's
        // HTML asset scanner extracts from `app/index.html` and ember-cli
        // blueprint files. Match is `spec.contains(substring)` per the
        // suppression logic in `find_unresolved_imports`.
        let substrings = EmberPlugin.generated_import_substrings();
        for spec in [
            // Handlebars placeholders in `<script src>` / `<link href>`.
            "./{{rootURL}}assets/cio.js",
            "./{{rootURL}}assets/vendor.css",
            "./{{config.assetsPath}}/app.js",
            // ember-cli blueprint scaffolds.
            "###APPNAME###/app",
            "###APPNAME###/router",
            "###DUMMY###/components/foo",
        ] {
            assert!(
                substrings.iter().any(|s| spec.contains(s)),
                "`{spec}` should match an Ember template-placeholder marker \
                 in {substrings:?}",
            );
        }
    }

    #[test]
    fn generated_import_substrings_do_not_match_legitimate_specifiers() {
        // Negative cases: real import shapes that must NOT match any
        // placeholder marker. Any false positive here would silence a real
        // unresolved-import bug.
        let substrings = EmberPlugin.generated_import_substrings();
        for spec in [
            "@ember/object",
            "@glimmer/component",
            "ember-source",
            "@ember-data/store",
            "./relative/path",
            "../parent/foo",
            "lodash/merge",
            "@scope/pkg/sub",
            "some-package",
            "ember",
        ] {
            assert!(
                !substrings.iter().any(|s| spec.contains(s)),
                "`{spec}` should NOT match any Ember template-placeholder \
                 marker in {substrings:?}",
            );
        }
    }
}
