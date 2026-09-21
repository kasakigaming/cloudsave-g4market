/**
 * Thanh trượt "liquid glass": rãnh mảnh, núm là viên kính trong làm cong phần
 * rãnh phía dưới (filter SVG `#liquid-glass` trong index.html).
 *
 * Giá trị đổi ngay khi kéo (để thứ đang chỉnh cập nhật trực tiếp), còn núm
 * đuổi theo bằng lò xo nên chuyển động mượt, và phình / kéo giãn theo vận tốc
 * như một giọt nước. Dùng được bằng bàn phím: mũi tên, PageUp/PageDown,
 * Home/End.
 */

export interface LiquidSlider {
  set(value: number): void;
}

interface Options {
  min: number;
  max: number;
  step: number;
  value: number;
  onInput(value: number): void;
}

const STIFFNESS = 0.22;
const DAMPING = 0.72;

export function liquidSlider(root: HTMLElement, opts: Options): LiquidSlider {
  const fill = root.querySelector<HTMLElement>(".lslider__fill")!;
  const thumb = root.querySelector<HTMLElement>(".lslider__thumb")!;
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  let value = opts.value;
  // Vị trí hiển thị của núm (0–1) và vận tốc của nó, cho lò xo.
  let pos = frac(value);
  let vel = 0;
  let dragging = false;
  let frame = 0;

  function frac(v: number) {
    return (v - opts.min) / (opts.max - opts.min);
  }

  function snap(v: number) {
    const s = Math.round((v - opts.min) / opts.step) * opts.step + opts.min;
    return Math.min(opts.max, Math.max(opts.min, s));
  }

  function render() {
    const w = root.clientWidth;
    const tw = thumb.offsetWidth;
    const x = pos * (w - tw);
    // Kéo nhanh thì giọt kính dài ra và dẹt lại; đang nhấn thì phình nhẹ.
    const stretch = Math.min(Math.abs(vel) * 6, 0.35);
    const press = dragging ? 1.12 : 1;
    thumb.style.transform = `translateX(${x}px) scale(${press * (1 + stretch)}, ${press * (1 - stretch * 0.55)})`;
    fill.style.width = `${x + tw / 2}px`;
  }

  function tick() {
    const target = frac(value);
    if (reduced) {
      pos = target;
      vel = 0;
    } else {
      vel = (vel + (target - pos) * STIFFNESS) * DAMPING;
      pos += vel;
    }
    render();
    if (Math.abs(target - pos) > 0.0005 || Math.abs(vel) > 0.0005 || dragging) {
      frame = requestAnimationFrame(tick);
    } else {
      pos = target;
      vel = 0;
      render();
      frame = 0;
    }
  }

  function kick() {
    if (!frame) frame = requestAnimationFrame(tick);
  }

  function update(v: number, emit: boolean) {
    const next = snap(v);
    root.setAttribute("aria-valuenow", String(next));
    root.setAttribute("aria-valuetext", `${next}%`);
    if (next !== value) {
      value = next;
      if (emit) opts.onInput(value);
    }
    kick();
  }

  function fromPointer(e: PointerEvent) {
    const r = root.getBoundingClientRect();
    const tw = thumb.offsetWidth;
    const f = (e.clientX - r.left - tw / 2) / (r.width - tw);
    update(opts.min + Math.min(1, Math.max(0, f)) * (opts.max - opts.min), true);
  }

  root.addEventListener("pointerdown", (e) => {
    dragging = true;
    root.classList.add("is-dragging");
    root.setPointerCapture(e.pointerId);
    root.focus();
    fromPointer(e);
  });
  root.addEventListener("pointermove", (e) => {
    if (dragging) fromPointer(e);
  });
  const end = () => {
    dragging = false;
    root.classList.remove("is-dragging");
    kick();
  };
  root.addEventListener("pointerup", end);
  root.addEventListener("pointercancel", end);

  root.addEventListener("keydown", (e) => {
    const big = (opts.max - opts.min) / 5;
    const delta: Record<string, number> = {
      ArrowRight: opts.step,
      ArrowUp: opts.step,
      ArrowLeft: -opts.step,
      ArrowDown: -opts.step,
      PageUp: big,
      PageDown: -big,
    };
    if (e.key in delta) update(value + delta[e.key], true);
    else if (e.key === "Home") update(opts.min, true);
    else if (e.key === "End") update(opts.max, true);
    else return;
    e.preventDefault();
  });

  // Kích thước cửa sổ đổi (hoặc trang Cài đặt vừa hiện ra) thì vẽ lại.
  new ResizeObserver(() => render()).observe(root);

  update(value, false);
  pos = frac(value);
  render();

  return {
    set(v: number) {
      update(v, false);
    },
  };
}
