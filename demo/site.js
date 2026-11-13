// Open Kioku site: copy buttons and the small-screen menu. Nothing else runs here.
(() => {
  const toggle = document.querySelector('.nav-toggle');
  const nav = document.querySelector('.nav');
  if (toggle && nav) {
    const set = (open) => {
      nav.toggleAttribute('data-open', open);
      toggle.setAttribute('aria-expanded', String(open));
    };
    toggle.addEventListener('click', () => set(!nav.hasAttribute('data-open')));
    nav.addEventListener('click', (event) => {
      if (event.target.closest('a')) set(false);
    });
    document.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') set(false);
    });
  }

  const write = async (text) => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      const area = document.createElement('textarea');
      area.value = text;
      area.setAttribute('readonly', '');
      area.style.position = 'fixed';
      area.style.opacity = '0';
      document.body.append(area);
      area.select();
      let done = false;
      try {
        done = document.execCommand('copy');
      } catch {
        done = false;
      }
      area.remove();
      return done;
    }
  };

  document.addEventListener('click', async (event) => {
    const button = event.target.closest('[data-copy]');
    if (!button) return;
    const label = button.textContent;
    const done = await write(button.getAttribute('data-copy'));
    button.textContent = done ? 'Copied' : 'Press Ctrl+C';
    button.toggleAttribute('data-copied', done);
    setTimeout(() => {
      button.textContent = label;
      button.removeAttribute('data-copied');
    }, 1600);
  });
})();
