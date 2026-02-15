// Light/Dark theme toggle
//
// Replaces mdBook's multi-theme dropdown (Coal/Navy/Aero/Rust/Light) with
// a single-click toggle between "light" and "navy" (dark) themes. Uses
// the universally recognized sun/moon icon convention:
//   - Moon icon in light mode  → "click to switch to dark"
//   - Sun icon in dark mode    → "click to switch to light"
//
// Relies on mdBook's built-in theme switching by programmatically clicking
// the hidden theme list items, ensuring localStorage and class updates
// are handled by mdBook's own code.

(function () {
    'use strict';

    var LIGHT = 'light';
    var DARK = 'navy';

    function getTheme() {
        try {
            return localStorage.getItem('mdbook-theme') || LIGHT;
        } catch (e) {
            return LIGHT;
        }
    }

    function isDark() {
        return getTheme() !== LIGHT;
    }

    function updateIcon(btn) {
        var icon = btn.querySelector('i');
        if (!icon) return;

        var dark = isDark();
        icon.className = dark ? 'fa fa-sun-o' : 'fa fa-moon-o';
        btn.title = dark ? 'Switch to light mode' : 'Switch to dark mode';
        btn.setAttribute('aria-label', btn.title);
        btn.setAttribute('aria-checked', dark ? 'true' : 'false');
    }

    function init() {
        var original = document.getElementById('theme-toggle');
        if (!original) return;

        // Clone button to strip mdBook's built-in event listeners
        var btn = original.cloneNode(true);
        original.parentNode.replaceChild(btn, original);

        // Update ARIA role: this is now a toggle, not a menu trigger
        btn.removeAttribute('aria-haspopup');
        btn.removeAttribute('aria-expanded');
        btn.removeAttribute('aria-controls');
        btn.setAttribute('role', 'switch');

        btn.addEventListener('click', function (e) {
            e.preventDefault();
            e.stopPropagation();

            var target = isDark() ? LIGHT : DARK;

            // Programmatically click the hidden theme list item so mdBook's
            // built-in theme switching (localStorage + class updates) runs
            var item = document.getElementById(target);
            if (item) {
                item.click();
            }

            setTimeout(function () {
                updateIcon(btn);
            }, 50);
        });

        updateIcon(btn);

        // Sync icon if theme changes via prefers-color-scheme or other means
        new MutationObserver(function () {
            updateIcon(btn);
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
