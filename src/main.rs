mod applications;
mod arguments;
mod x11;

use applications::{read_applications, Apps};
use arguments::{get_args, Args};
use std::cmp::{max, min};
use std::process::exit;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use x11::{Action, GraphicsContext, TextRenderingContext, X11Context};
use x11_dl::xlib;

const KEY_ESCAPE: u32 = 9;
const KEY_LEFT: u32 = 113;
const KEY_RIGHT: u32 = 114;
const KEY_BACKSPACE: u32 = 22;
const KEY_ENTER: u32 = 36;
const KEY_TAB: u32 = 23;

struct State {
    caret_pos: i32,
    text: String,
    suggestions: Vec<(String, usize)>,
    selected: u8,
}

fn main() {
    let args = get_args();
    // spawn a thread for reading all applications
    let apps = Arc::new(Mutex::new(Apps::new()));
    let apps_clone = apps.clone();
    let path = args.path;
    thread::spawn(move || read_applications(&apps_clone, path));

    let mut state = State {
        caret_pos: 0,
        text: String::new(),
        suggestions: Vec::new(),
        selected: 0,
    };

    // initialize xlib context
    let xc = match X11Context::new() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {:?}", e);
            exit(1);
        }
    };

    // get screen width and the position where to map window
    let mut screen_width = 0;
    let mut window_pos = (0, 0);

    let mouse_pos = xc.get_mouse_pos();
    for screen in xc.get_screens() {
        // multiple monitors support
        if in_rect(
            (mouse_pos.0, mouse_pos.1),
            (screen.x_org, screen.y_org),
            (screen.width, screen.height),
        ) {
            screen_width = screen.width as u32;
            window_pos.0 = screen.x_org as i32;
            window_pos.1 = if args.bottom {
                screen.y_org as i32 + screen.height as i32 - args.height as i32
            } else {
                screen.y_org as i32
            };
            break;
        }
    }

    // create the window
    let window = xc.create_window(window_pos, screen_width, args.height);

    xc.grab_keyboard();

    let font_height = {
        let mut h = 12;
        for x in args.font.split(':') {
            if x.starts_with("size=") {
                h = (&x[5..]).parse().expect("couldn't parse font size");
                break;
            }
        }
        h
    };
    let mut trc = xc.init_trc(&window, &format!("{}:size=12:antialias=true", args.font));
    xc.add_color_to_trc(&mut trc, args.color2);
    xc.add_color_to_trc(&mut trc, args.color3);

    let gc = xc.init_gc(&window);

    // show window
    xc.map_window(&window);

    xc.run(|xc, event| {
        update_suggestions(&xc, &trc, &mut state, screen_width, &apps);
        render_bar(&xc, &trc, &gc, screen_width, &state, &args, font_height);
        match event {
            None => Action::Run,
            Some(e) => handle_event(&xc, e, &mut state, &apps, &args.terminal),
        }
    });
}

fn render_bar(
    xc: &X11Context,
    trc: &TextRenderingContext,
    gc: &GraphicsContext,
    width: u32,
    state: &State,
    args: &Args,
    font_height: i32,
) {
    let text_y = args.height as i32 / 2 + font_height / 2;
    // clear
    xc.draw_rect(&gc, args.color0, 0, 0, width, args.height);

    // render the typed text
    xc.render_text(&trc, 0, 0, text_y, &state.text);
    // and the caret
    xc.draw_rect(
        &gc,
        args.color2,
        xc.get_text_dimensions(&trc, &state.text[0..state.caret_pos as usize])
            .0 as i32,
        2,
        2,
        args.height - 4,
    );

    // render suggestions
    let mut x = (width as f32 * 0.3).floor() as i32;
    for (i, suggestion) in state.suggestions.iter().enumerate() {
        let name = &suggestion.0;
        let name_width = xc.get_text_dimensions(&trc, &name).0 as i32;
        // if selected, render rectangle below
        if state.selected as usize == i {
            xc.draw_rect(&gc, args.color1, x, 0, name_width as u32 + 16, args.height);
        }

        xc.render_text(&trc, 1, x + 8, text_y, name);

        x += name_width + 16;
    }
}

fn update_suggestions(
    xc: &X11Context,
    trc: &TextRenderingContext,
    state: &mut State,
    width: u32,
    apps: &Mutex<applications::Apps>,
) {
    state.suggestions.clear();
    // iterate over application names
    // and find those that contain the typed text
    let mut x = 0;
    let max_width = (width as f32 * 0.7).floor() as i32;
    let apps_lock = apps.lock().unwrap();
    for i in 0..(*apps_lock).len() {
        let name = &apps_lock[i].name;
        if name.to_lowercase().contains(&state.text.to_lowercase()) {
            let width = xc.get_text_dimensions(&trc, &name).0 as i32;
            if x + width <= max_width {
                x += width;
                state.suggestions.push((apps_lock[i].name.clone(), i));
            } else {
                break;
            }
        }
    }
}

fn handle_event(
    xc: &X11Context,
    event: &xlib::XEvent,
    state: &mut State,
    apps: &Mutex<applications::Apps>,
    terminal: &str,
) -> Action {
    if let Some(e) = xc.xevent_to_xkeyevent(*event) {
        match e.keycode {
            KEY_ESCAPE => {
                return Action::Stop;
            }
            KEY_LEFT => {
                if state.selected == 0 {
                    state.caret_pos = max(0, state.caret_pos - 1);
                } else {
                    state.selected -= 1;
                }
            }
            KEY_RIGHT => {
                if state.caret_pos == state.text.len() as i32 {
                    state.selected = min(state.selected + 1, state.suggestions.len() as u8 - 1);
                } else {
                    state.caret_pos += 1;
                }
            }
            KEY_BACKSPACE => {
                if state.caret_pos != 0 {
                    state.text.remove(state.caret_pos as usize - 1);
                    state.caret_pos -= 1;
                    state.selected = 0;
                }
            }
            KEY_ENTER => {
                // if no suggestions available, just run the text, otherwise launch selected application
                if state.suggestions.is_empty() {
                    run_command(&state.text);
                } else {
                    let app = &apps.lock().unwrap()[state.suggestions[state.selected as usize].1];
                    if app.show_terminal {
                        run_command(&format!("{} -e \"{}\"", terminal, app.exec));
                    } else {
                        run_command(&app.exec);
                    }
                }
                return Action::Stop;
            }
            KEY_TAB => {
                if !state.suggestions.is_empty() {
                    state.text = state.suggestions[state.selected as usize].0.to_string();
                    state.caret_pos = state.text.len() as i32;
                    state.selected = 0;
                }
            }
            _ => {
                // some other key
                // try to interpret the key as a character
                let c = xc.keyevent_to_char(e);
                if !c.is_ascii_control() {
                    state.text.insert(state.caret_pos as usize, c);
                    state.caret_pos += 1;
                    state.selected = 0;
                }
            }
        }
    }
    Action::Run
}

fn run_command(command: &str) {
    let mut parts = command.split(' ');
    if !command.is_empty() {
        let mut c = Command::new(parts.next().unwrap());
        c.args(parts);
        let _ = c.spawn();
    }
}

fn in_rect(point: (i32, i32), rect: (i16, i16), rect_size: (i16, i16)) -> bool {
    if point.0 >= rect.0 as i32
        && point.0 <= (rect.0 + rect_size.0) as i32
        && point.1 >= rect.1 as i32
        && point.1 <= (rect.1 + rect_size.1) as i32
    {
        return true;
    }
    false
}
