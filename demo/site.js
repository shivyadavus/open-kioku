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
  const revealables=document.querySelectorAll('.section-head,.feature,.security-card,.big-proof,.proof-stack>div,.trustbar,.video-card,.architecture,.cta-box,.tool,.step,.install-card,.readout,.readout-notes');
  const io=new IntersectionObserver(entries=>{entries.forEach(e=>{if(e.isIntersecting){e.target.classList.add('visible');io.unobserve(e.target)}})},{threshold:.12,rootMargin:'0px 0px -8% 0px'});
  revealables.forEach((el,i)=>{el.classList.add('reveal');el.style.transitionDelay=`${Math.min((i%4)*60,180)}ms`;io.observe(el)});
}

// ── Live evidence terminal ───────────────────────────────────────────────────
const live=document.getElementById('live-proof');
if(live){
  // Scene 1 is actual `ok context` output on this repository (README, "First Win"), trimmed.
  // Scene 2 is the verify failure from demo.tape (one drive-by edit outside the declared boundary).
  // Scene 3 replays the frozen fixture's java-no-gold-password-reset case (benchmarks/retrieval-cases.json): 5 of 5 no-gold tasks come back Low.
  const scenes=[
    {cmd:'ok context "reap the doctor\'s MCP probe child process" --format markdown',lines:[
      ['section','CONFIDENCE'],
      ['row','overall','<span class="warn">Medium</span> (0.74) · exact_references 0.25 · task_relevance 0.83 …'],
      ['row','caveats','<span class="warn">exact symbol/reference evidence is absent · runtime corroboration is absent</span>'],
      ['section','RETRIEVAL'],
      ['row','attempted','lexical · document · exact_semantic · graph · validation · git_history · runtime'],
      ['row','succeeded','lexical · document · exact_semantic · graph · validation · git_history'],
      ['row','exact-authority','0 selections · retrieval confidence Medium'],
      ['row','caveats','<span class="warn">no runtime traces, logs, or incidents are ingested for this repository</span>'],
      ['section','PRIMARY CONTEXT'],
      ['row','1','<span class="json-string">crates/open-kioku-cli/src/reports/status_setup_doctor.rs</span> lines 1-107'],
      ['row','2','<span class="json-string">crates/open-kioku-cli/src/commands/onboarding.rs</span> lines 2-35'],
      ['comment','# the commit that made this change touched exactly one file; it is the first result'],
      ['comment','# no SCIP index and no runtime artifacts here, so the label is Medium, not Exact']]},
    {cmd:'ok verify --plan plan.json --git',lines:[
      ['warn','[out_of_boundary] go/shipping/carrier.go: path is outside the saved plan boundary'],
      ['comment','# one edit inside the declared boundary, one drive-by outside it: the check fails'],
      ['comment','# a green exit code from a test runner is not proof the right files changed; this is']]},
    {cmd:'ok context "password reset should email a one-time code and throttle repeated reset attempts" --format markdown',lines:[
      ['section','CONFIDENCE'],
      ['row','overall','<span class="warn">Low</span>'],
      ['out','candidates are still listed; the pack tells the caller not to trust them rather than returning nothing'],
      ['comment','# on the frozen 30-case fixture, 5 of 5 no-gold tasks come back Low (benchmarks/retrieval-baseline.json)']]}
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

// ── Evidence console: type the commands, reveal the output, then stay static ─
(function(){
  const con=document.getElementById('console');
  if(!con||reduceMotion)return;
  const lines=[...con.querySelectorAll('.cl')];
  if(!lines.length)return;
  con.classList.add('play');
  let i=0;
  function next(){
    if(i>=lines.length){con.classList.remove('play');con.classList.add('done');return}
    const line=lines[i++];
    const cmd=line.querySelector('.cmd-text');
    if(cmd){
      const text=cmd.textContent;let n=0;cmd.textContent='';
      const caret=document.createElement('span');caret.className='caret';caret.setAttribute('aria-hidden','true');cmd.after(caret);
      line.classList.add('on');
      const t=setInterval(()=>{n++;cmd.textContent=text.slice(0,n);if(n>=text.length){clearInterval(t);caret.remove();setTimeout(next,260)}},34);
    }else{
      line.classList.add('on');
      setTimeout(next,line.classList.contains('term-section')||line.classList.contains('evidence-card')?170:95);
    }
  }
  setTimeout(next,420);
})();

// ── Pipeline rail: stroke-draw once when it scrolls into view ───────────────
(function(){
  const pipeline=document.querySelector('.pipeline');
  if(!pipeline||reduceMotion||!('IntersectionObserver' in window))return;
  pipeline.querySelectorAll('.pipe').forEach((p,i)=>p.style.setProperty('--i',i));
  pipeline.classList.add('armed');
  const io=new IntersectionObserver(entries=>{entries.forEach(e=>{if(e.isIntersecting){pipeline.classList.add('lit');io.disconnect()}})},{threshold:.4});
  io.observe(pipeline);
})();

// ── Install matrix: segmented control over the channel panels ───────────────
(function(){
  const seg=document.querySelector('.seg');
  if(!seg)return;
  const btns=[...seg.querySelectorAll('.seg-btn')];
  const panels=btns.map(b=>document.getElementById(b.getAttribute('aria-controls')));
  function pick(idx,{focus=false}={}){
    btns.forEach((b,i)=>{const on=i===idx;b.setAttribute('aria-selected',String(on));b.tabIndex=on?0:-1;if(panels[i])panels[i].hidden=!on});
    if(focus)btns[idx].focus();
  }
  btns.forEach((b,i)=>{
    b.addEventListener('click',()=>pick(i));
    b.addEventListener('keydown',e=>{let n=i;if(e.key==='ArrowRight')n=(i+1)%btns.length;else if(e.key==='ArrowLeft')n=(i-1+btns.length)%btns.length;else if(e.key==='Home')n=0;else if(e.key==='End')n=btns.length-1;else return;e.preventDefault();pick(n,{focus:true})});
  });
  pick(0);
})();
