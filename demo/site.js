const steps={
plan:{title:'ok plan · representative output',html:`<div><span class="prompt">$</span> ok --json plan "change token expiration"</div><br><div class="good">✓ 4 primary context items</div><div class="good">✓ 4 direct impact candidates</div><div class="good">✓ 5 validation candidates</div><div class="result-title">risk: <span class="warn">medium · 0.31</span></div><div><span class="json-key">primary:</span> <span class="json-string">src/auth.rs</span>, <span class="json-string">tests/auth_flow.rs</span>, <span class="json-string">src/lib.rs</span>, <span class="json-string">README.md</span></div><br><div class="warn">! exact references, runtime, history, and coverage were unavailable</div><div class="comment"># Missing evidence lowers confidence instead of inventing certainty.</div>`},
impact:{title:'impact · from the real plan',html:`<div><span class="prompt">$</span> plan.impact.direct_impacts</div><br><div class="result-title">4 direct impact candidates</div><div><span class="json-string">src/lib.rs:7-12</span> <span class="comment"># handle_login</span></div><div><span class="json-string">tests/auth_flow.rs:4-7</span> <span class="comment"># login_returns_valid_token</span></div><div><span class="json-string">src/lib.rs:3-6</span> <span class="comment"># RequestContext</span></div><div><span class="json-string">src/lib.rs:1-2</span> <span class="comment"># auth module</span></div><br><div class="warn">! evidence quality caveat: exact symbol/reference evidence unavailable</div>`},
tests:{title:'validation · from the real plan',html:`<div><span class="prompt">$</span> plan.validation</div><br><div class="result-title">5 runnable candidates · command: <span class="json-string">cargo test</span></div><div><span class="good">01</span> issue_token</div><div><span class="good">02</span> issues_token_with_user_id</div><div><span class="good">03</span> login_returns_valid_token</div><div><span class="good">04</span> tests</div><div><span class="good">05</span> validate_token</div><br><div class="comment"># Selected before the source edit from indexed repository evidence.</div>`},
verify:{title:'ok verify · real attested run',html:`<div><span class="prompt">$</span> ok --json verify --plan plan.json --changed src/lib.rs --run-commands</div><br><div class="good">✓ cargo test · passed · exit 0</div><div class="good">✓ 2 tests passed · 0 failed</div><div class="good">✓ boundary violations: 0</div><div class="good">✓ validation attestation recorded</div><br><div class="result-title">verdict: <span class="warn">WARN</span></div><div class="warn">! exact references / runtime / history / coverage unavailable</div><div class="comment"># Tests passed. Open Kioku still refused to overstate confidence.</div>`}
};
const out=document.getElementById('tour-output'),title=document.getElementById('tour-title');
function render(step){if(!out||!title)return;out.classList.remove('fade');void out.offsetWidth;out.innerHTML=steps[step].html;title.textContent=steps[step].title;out.classList.add('fade')}
const tabs=[...document.querySelectorAll('.tab')];
function selectTab(btn,{focus=false}={}){tabs.forEach(b=>{const selected=b===btn;b.classList.toggle('active',selected);b.setAttribute('aria-selected',String(selected));b.tabIndex=selected?0:-1});out.setAttribute('aria-labelledby',btn.id);render(btn.dataset.step);if(focus)btn.focus()}
tabs.forEach((btn,index)=>{btn.addEventListener('click',()=>selectTab(btn));btn.addEventListener('keydown',event=>{let next=index;if(event.key==='ArrowDown'||event.key==='ArrowRight')next=(index+1)%tabs.length;else if(event.key==='ArrowUp'||event.key==='ArrowLeft')next=(index-1+tabs.length)%tabs.length;else if(event.key==='Home')next=0;else if(event.key==='End')next=tabs.length-1;else return;event.preventDefault();selectTab(tabs[next],{focus:true})})});

if(out&&title)render('plan');
document.querySelectorAll('[data-copy]').forEach(btn=>btn.addEventListener('click',async()=>{const original=btn.textContent;try{await navigator.clipboard.writeText(btn.dataset.copy);btn.textContent='Copied';}catch(e){btn.textContent='Select';}setTimeout(()=>btn.textContent=original,1400)}));
const navToggle=document.querySelector('.nav-toggle'),nav=document.getElementById('primary-nav');
if(navToggle&&nav){const closeNav=()=>{nav.classList.remove('open');navToggle.setAttribute('aria-expanded','false');navToggle.querySelector('.sr-only').textContent='Open navigation'};navToggle.addEventListener('click',()=>{const open=!nav.classList.contains('open');nav.classList.toggle('open',open);navToggle.setAttribute('aria-expanded',String(open));navToggle.querySelector('.sr-only').textContent=open?'Close navigation':'Open navigation'});nav.querySelectorAll('a').forEach(link=>link.addEventListener('click',closeNav));document.addEventListener('keydown',event=>{if(event.key==='Escape')closeNav()})}
if(window.matchMedia('(prefers-reduced-motion: reduce)').matches){document.querySelectorAll('video[autoplay]').forEach(video=>{video.pause();video.removeAttribute('autoplay')})}

// ── Scroll reveal ────────────────────────────────────────────────────────────
const reduceMotion=window.matchMedia('(prefers-reduced-motion: reduce)').matches;
if(!reduceMotion&&'IntersectionObserver' in window){
  const revealables=document.querySelectorAll('.section-head,.feature,.security-card,.big-proof,.proof-stack>div,.trustbar>div,.video-card,.architecture,.cta-box');
  const io=new IntersectionObserver(entries=>{entries.forEach(e=>{if(e.isIntersecting){e.target.classList.add('visible');io.unobserve(e.target)}})},{threshold:.12,rootMargin:'0px 0px -8% 0px'});
  revealables.forEach((el,i)=>{el.classList.add('reveal');el.style.transitionDelay=`${Math.min((i%4)*60,180)}ms`;io.observe(el)});
}

// ── Live evidence terminal ───────────────────────────────────────────────────
const live=document.getElementById('live-proof');
if(live){
  const scenes=[
    {cmd:'ok plan "change token expiration"',lines:[
      ['good','✓ index ready · generation g1756-4f2a · 247,499 symbols'],
      ['section','PRE-EDIT PLAN'],
      ['row','context','<span class="json-string">src/auth.rs</span> · <span class="json-string">src/lib.rs</span> · <span class="json-string">tests/auth_flow.rs</span>'],
      ['row','impact','2 structurally proven dependents · 1 possible (heuristic)'],
      ['row','tests','<span class="good">issue_token</span> · <span class="good">validate_token</span> — required by coverage evidence'],
      ['row','boundary','source + matching tests only'],
      ['out','exact lookup answered in <span class="good">0.02s</span>']]},
    {cmd:'ok verify --plan plan.json --changed src/auth.rs tests/auth_flow.rs',lines:[
      ['good','✓ cargo test · 2 passed · 0 failed'],
      ['good','✓ boundary violations: 0'],
      ['good','✓ change stayed inside the planned boundary'],
      ['out','verdict: <span class="warn">WARN</span> — runtime evidence absent; confidence not overstated']]},
    {cmd:'ok context "migrate the billing webhooks"',lines:[
      ['warn','! calibrated_cc6_abstention: only 1 independent retrieval stream supports the top result'],
      ['out','This repository has no billing webhooks. Open Kioku says so'],
      ['out','instead of returning confident-looking noise.'],
      ['comment','# insufficient evidence ≠ an answer']]}
  ];
  const esc=t=>t;
  function renderScene(i,typed){
    const s=scenes[i];
    let html=`<div class="cmd"><span class="prompt">$</span> ${esc(s.cmd.slice(0,typed))}${typed<s.cmd.length?'<span class="cursor-blink"></span>':''}</div>`;
    if(typed>=s.cmd.length){
      const shown=Math.floor((typed-s.cmd.length)/6);
      s.lines.slice(0,shown).forEach(l=>{
        if(l[0]==='section')html+=`<div class="term-section">${l[1]}</div>`;
        else if(l[0]==='row')html+=`<div class="term-row"><span class="dim">${l[1]}</span><span>${l[2]}</span></div>`;
        else html+=`<div class="${l[0]}">${l[1]}</div>`;
      });
      if(shown>=s.lines.length)html+='<div style="margin-top:12px"><span class="prompt">$</span> <span class="cursor-blink"></span></div>';
    }
    live.innerHTML=html;
    return typed>=s.cmd.length&&Math.floor((typed-s.cmd.length)/6)>=s.lines.length;
  }
  if(reduceMotion){
    // Static full render of the first scene.
    renderScene(0,Number.MAX_SAFE_INTEGER);
  }else{
    let scene=0,tick=0,holdUntil=0;
    setInterval(()=>{
      const now=Date.now();
      if(holdUntil){if(now<holdUntil)return;holdUntil=0;scene=(scene+1)%scenes.length;tick=0}
      tick+=2;
      if(renderScene(scene,tick))holdUntil=now+3600;
    },50);
  }
}

/* Code rain: quiet falling glyph columns behind the hero. Fades out below the
   fold via the CSS mask; disabled for reduced motion and narrow screens. */
(function () {
  var canvas = document.getElementById('code-rain');
  if (!canvas) return;
  var reduced = window.matchMedia('(prefers-reduced-motion: reduce)');
  if (reduced.matches || window.innerWidth < 720) { canvas.remove(); return; }
  var ctx = canvas.getContext('2d');
  if (!ctx) { canvas.remove(); return; }

  var GLYPHS = '{}[]()<>=;:./|&+-*#$_~fnokletifpubuse01';
  var FONT_SIZE = 13;
  var COL_GAP = 34;
  var dpr, cols, drops;

  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.floor(window.innerWidth * dpr);
    canvas.height = Math.floor(window.innerHeight * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.font = FONT_SIZE + 'px ' + '"JetBrains Mono",Menlo,monospace';
    cols = Math.ceil(window.innerWidth / COL_GAP);
    drops = [];
    for (var i = 0; i < cols; i++) {
      drops.push({
        y: Math.random() * window.innerHeight * 1.2 - window.innerHeight * 0.2,
        speed: 22 + Math.random() * 34,
        mint: Math.random() < 0.22
      });
    }
  }

  var last = 0;
  function tick(now) {
    if (now - last > 66) { // ~15fps is plenty for this density
      last = now;
      ctx.globalCompositeOperation = 'destination-out';
      ctx.fillStyle = 'rgba(0,0,0,0.16)';
      ctx.fillRect(0, 0, window.innerWidth, window.innerHeight);
      ctx.globalCompositeOperation = 'source-over';
      for (var i = 0; i < cols; i++) {
        var d = drops[i];
        d.y += d.speed * 0.066;
        if (d.y > window.innerHeight + 40) {
          d.y = -20 - Math.random() * 300;
          d.speed = 22 + Math.random() * 34;
          d.mint = Math.random() < 0.22;
        }
        var ch = GLYPHS.charAt((Math.random() * GLYPHS.length) | 0);
        ctx.fillStyle = d.mint ? 'rgba(87,230,196,0.5)' : 'rgba(160,175,195,0.28)';
        ctx.fillText(ch, i * COL_GAP + 8, d.y);
      }
    }
    raf = requestAnimationFrame(tick);
  }

  var raf;
  resize();
  window.addEventListener('resize', resize);
  reduced.addEventListener && reduced.addEventListener('change', function (e) {
    if (e.matches) { cancelAnimationFrame(raf); canvas.remove(); }
  });
  raf = requestAnimationFrame(tick);
})();
