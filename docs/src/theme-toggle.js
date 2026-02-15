// Light/Dark theme toggle for mdBook
//
// Replaces mdBook's multi-theme dropdown (Coal/Navy/Ayu/Rust/Light) with
// a single-click toggle between "light" and "navy" (dark) themes. Uses
// the universally recognized sun/moon icon convention:
//   - Moon icon in light mode  → "click to switch to dark"
//   - Sun icon in dark mode    → "click to switch to light"
//
// Self-contained: sets localStorage and the <html> class directly rather
// than delegating to mdBook's internal theme list handlers. This avoids
// breakage when mdBook's DOM structure or event wiring changes between
// versions.

(function () {
    'use strict';

    var LIGHT = 'light';
    var DARK = 'navy';
    var ALL_THEMES = ['light', 'rust', 'coal', 'navy', 'ayu'];

    function getCurrentTheme() {
        try {
            return localStorage.getItem('mdbook-theme') || LIGHT;
        } catch (e) {
            return LIGHT;
        }
    }

    function isDark() {
        return getCurrentTheme() !== LIGHT;
    }

    function setTheme(theme) {
        // Persist selection
        try {
            localStorage.setItem('mdbook-theme', theme);
        } catch (e) { /* localStorage unavailable */ }

        // mdBook scopes all theme CSS to a class on <html>.
        // Remove every known theme class, then apply the target.
        ALL_THEMES.forEach(function (t) {
            document.documentElement.classList.remove(t);
        });
        document.documentElement.classList.add(theme);
    }

    function updateButton(btn) {
        var icon = btn.querySelector('i');
        if (!icon) return;

        var dark = isDark();
        icon.className = dark ? 'fa fa-sun-o' : 'fa fa-moon-o';
        btn.title = dark ? 'Switch to light mode' : 'Switch to dark mode';
        btn.setAttribute('aria-label', btn.title);
        btn.setAttribute('aria-checked', String(dark));
    }

    function init() {
        var original = document.getElementById('theme-toggle');
        if (!original) return;

        // Clone to strip mdBook's built-in event listeners (popup toggle,
        // aria-expanded management) that conflict with our toggle behavior.
        var btn = original.cloneNode(true);
        original.parentNode.replaceChild(btn, original);

        // Enforce light-or-navy only. If the user previously selected a
        // theme we no longer expose (rust, coal, ayu), reset to light.
        var current = getCurrentTheme();
        if (current !== LIGHT && current !== DARK) {
            setTheme(LIGHT);
        }

        // ARIA: this is a binary toggle, not a menu trigger
        btn.removeAttribute('aria-haspopup');
        btn.removeAttribute('aria-expanded');
        btn.removeAttribute('aria-controls');
        btn.setAttribute('role', 'switch');

        btn.addEventListener('click', function (e) {
            e.preventDefault();
            e.stopPropagation();

            setTheme(isDark() ? LIGHT : DARK);
            updateButton(btn);
        });

        updateButton(btn);

        // Sync icon if the theme class changes externally (e.g., a
        // prefers-color-scheme media query handler in mdBook's own JS)
        new MutationObserver(function () {
            updateButton(btn);
        }).observe(document.documentElement, {
            attributes: true,
            attributeFilter: ['class']
        });
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
