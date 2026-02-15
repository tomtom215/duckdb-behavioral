// mdBook toolbar and content customization
//
// 1. Theme toggle: replaces multi-theme dropdown with light/dark toggle
// 2. Print override: calls window.print() instead of navigating to print.html
// 3. Table wrapping: wraps tables in scrollable containers for mobile
//
// Self-contained: sets localStorage and <html> class directly. Does not
// depend on mdBook's internal theme list handlers or DOM structure.

(function () {
    'use strict';

    // ── Theme toggle ───────────────────────────────────────────────────

    var LIGHT = 'light';
    var DARK = 'navy';
    var ALL_THEMES = ['light', 'rust', 'coal', 'navy', 'ayu'];
    var lastToggle = 0;

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
        try {
            localStorage.setItem('mdbook-theme', theme);
        } catch (e) { /* localStorage unavailable (private browsing) */ }

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

    function setupThemeToggle() {
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

        // iOS Safari: eliminate 300ms tap delay
        btn.style.touchAction = 'manipulation';
        btn.style.webkitTapHighlightColor = 'transparent';

        btn.addEventListener('click', function (e) {
            e.preventDefault();
            e.stopPropagation();

            // Debounce: prevent double-fire from touch + click on mobile
            var now = Date.now();
            if (now - lastToggle < 300) return;
            lastToggle = now;

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

    // ── Print button override ──────────────────────────────────────────
    // mdBook's print button navigates to print.html, which auto-calls
    // window.print(). Canceling then refreshing re-triggers the dialog.
    // Override: call window.print() on the current page, no navigation.

    function setupPrintOverride() {
        var icon = document.getElementById('print-button');
        if (!icon) return;

        var link = icon.closest ? icon.closest('a') : icon.parentElement;
        if (!link || link.tagName !== 'A') return;

        link.addEventListener('click', function (e) {
            e.preventDefault();
            window.print();
        });

        // Update accessibility to reflect new behavior
        link.setAttribute('title', 'Print this page');
        link.setAttribute('aria-label', 'Print this page');
    }

    // ── Table wrappers ─────────────────────────────────────────────────
    // Wrap tables in scrollable containers. This is more reliable than
    // display:block on <table> because it preserves natural table layout
    // while containing overflow in a dedicated scroll viewport.

    function wrapTables() {
        var tables = document.querySelectorAll('.content table');
        for (var i = 0; i < tables.length; i++) {
            var table = tables[i];
            // Skip if already wrapped
            if (table.parentElement &&
                table.parentElement.classList.contains('table-wrapper')) continue;

            var wrapper = document.createElement('div');
            wrapper.className = 'table-wrapper';
            table.parentNode.insertBefore(wrapper, table);
            wrapper.appendChild(table);
        }
    }

    // ── Initialization ─────────────────────────────────────────────────

    function init() {
        setupThemeToggle();
        setupPrintOverride();
        wrapTables();
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
