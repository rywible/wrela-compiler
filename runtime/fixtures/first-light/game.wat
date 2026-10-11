;; first-light: a hand-written wrela program, for developing and testing the hosts before the
;; compiler can emit programs. It speaks the program ABI and command stream v7 (runtime/abi).
;;
;; On its first frame it creates buffer 1 (a 64-entry RGBA8 palette), writes four seed colours
;; into it, and dispatches the `fill` compute pipeline (1), which derives the other 60 entries.
;; Every frame it computes a circle's centre on the CPU from the time, then submits a screen
;; pass: one full-screen triangle drawn by the `first-light` render pipeline (0), which reads the
;; palette and colours each pixel by its distance to the circle.
;;
;; `game.wasm` is this file compiled by the `wat` crate; runtime/native/tests/suite/fixture.rs
;; checks it's current (WRELA_BLESS=1 rewrites it).
(module
  (import "wrela" "submit" (func $submit (param i32 i32)))
  (import "wrela" "memory" (memory 1 16384 shared)) (export "memory" (memory 0))

  ;; Set once the first frame has submitted the setup batch.
  (global $ready (mut i32) (i32.const 0))

  ;; The setup batch: bytes 0..124.
  (data (i32.const 0)
    "WRCS" "\08\00\00\00" "\70\00\00\00"            ;; magic, version 8, body length 112
    ;; CreateBuffer handle 1, 256 bytes
    "\01\00\00\00" "\08\00\00\00" "\01\00\00\00" "\00\01\00\00"
    ;; WriteBuffer handle 1, offset 0, 16 bytes: the seed colours (RGBA8, R in the low byte)
    "\02\00\00\00" "\1c\00\00\00" "\01\00\00\00" "\00\00\00\00" "\10\00\00\00"
    "\10\18\30\ff"                                   ;; 0: background, top
    "\e0\70\20\ff"                                   ;; 1: the disc
    "\30\10\40\ff"                                   ;; 2: background, bottom
    "\f0\c0\60\ff"                                   ;; 3: the glow
    ;; Dispatch pipeline 1, 1x1x1 groups, one binding (buffer 1, bytes 0..256), 16 uniform bytes
    "\03\00\00\00" "\34\00\00\00"
    "\01\00\00\00" "\01\00\00\00" "\01\00\00\00" "\01\00\00\00"
    "\01\00\00\00" "\01\00\00\00" "\00\00\00\00" "\00\01\00\00" "\10\00\00\00"
    "\08\04\02\00" "\40\00\00\00" "\00\00\00\00" "\00\00\00\00")  ;; step 0x00020408, count 64

  ;; The frame batch: bytes 256..372. `frame` fills in the draw's uniform block (332..364).
  (data (i32.const 256)
    "WRCS" "\08\00\00\00" "\68\00\00\00"            ;; magic, version 8, body length 104
    ;; BeginScreenPass, clear colour (0.02, 0.03, 0.06, 1.0)
    "\04\00\00\00" "\10\00\00\00"
    "\0a\d7\a3\3c" "\8f\c2\f5\3c" "\8f\c2\75\3d" "\00\00\80\3f"
    ;; Draw pipeline 0, 3 vertices, 1 instance, one binding (buffer 1, bytes 0..256), 32 uniform
    ;; bytes
    "\05\00\00\00" "\40\00\00\00"
    "\00\00\00\00" "\03\00\00\00" "\01\00\00\00"
    "\01\00\00\00" "\01\00\00\00" "\00\00\00\00" "\00\01\00\00" "\20\00\00\00"
    "\00\00\00\00" "\00\00\00\00"                    ;; 332: resolution
    "\00\00\00\00" "\00\00\00\00"                    ;; 340: centre
    "\00\00\00\00" "\00\00\00\00"                    ;; 348: time, radius
    "\00\00\00\00" "\00\00\00\00"                    ;; 356: padding
    ;; Present
    "\06\00\00\00" "\00\00\00\00")

  ;; A triangle wave with period 1: -1 at whole numbers, 1 halfway between.
  (func $tri (export "tri") (param $x f32) (result f32)
    (f32.sub
      (f32.mul
        (f32.const 4)
        (f32.abs (f32.sub (local.get $x) (f32.floor (f32.add (local.get $x) (f32.const 0.5))))))
      (f32.const 1)))

  (func (export "frame") (param $time f32) (param $width i32) (param $height i32)
    (local $w f32) (local $h f32) (local $phase f32)
    (if (i32.eqz (global.get $ready))
      (then
        (call $submit (i32.const 0) (i32.const 124))
        (global.set $ready (i32.const 1))))
    (local.set $w (f32.convert_i32_u (local.get $width)))
    (local.set $h (f32.convert_i32_u (local.get $height)))
    (local.set $phase (f32.mul (local.get $time) (f32.const 0.25)))
    (f32.store (i32.const 332) (local.get $w))
    (f32.store (i32.const 336) (local.get $h))
    ;; centre = (w * (0.5 + 0.3 tri(phase)), h * (0.5 + 0.2 tri(phase + 0.25)))
    (f32.store (i32.const 340)
      (f32.mul (local.get $w)
        (f32.add (f32.const 0.5) (f32.mul (f32.const 0.3) (call $tri (local.get $phase))))))
    (f32.store (i32.const 344)
      (f32.mul (local.get $h)
        (f32.add (f32.const 0.5)
          (f32.mul (f32.const 0.2)
            (call $tri (f32.add (local.get $phase) (f32.const 0.25)))))))
    (f32.store (i32.const 348) (local.get $time))
    ;; radius = 0.15 min(w, h)
    (f32.store (i32.const 352)
      (f32.mul (f32.const 0.15) (f32.min (local.get $w) (local.get $h))))
    (call $submit (i32.const 256) (i32.const 116))))
