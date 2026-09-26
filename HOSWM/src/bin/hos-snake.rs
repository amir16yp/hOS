//! A tiny Snake game controlled by arrow keys or WASD.
use hoswm::{
    client::{Client, Event, WINDOW_DEFER_CLOSE, WINDOW_RAW_INPUT},
    font::Font,
    surface::Surface,
};
use std::{
    collections::VecDeque,
    thread,
    time::{Duration, Instant},
};

const BG: u32 = 0xff101815;
const PANEL: u32 = 0xff1b2b25;
const SNAKE: u32 = 0xff72dbac;
const FOOD: u32 = 0xffef6976;
const TEXT: u32 = 0xffdfe8e2;
const COLS: i32 = 24;
const ROWS: i32 = 18;
const CELL: i32 = 14;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

struct Game {
    body: VecDeque<(i32, i32)>,
    direction: Direction,
    next_direction: Direction,
    food: (i32, i32),
    seed: u32,
    score: u32,
    over: bool,
}

impl Game {
    fn new() -> Self {
        let mut game = Self {
            body: VecDeque::new(),
            direction: Direction::Right,
            next_direction: Direction::Right,
            food: (10, 8),
            seed: 0x1234_5678,
            score: 0,
            over: false,
        };
        game.reset();
        game
    }

    fn reset(&mut self) {
        self.body.clear();
        self.body.extend([(5, 8), (4, 8), (3, 8)]);
        self.direction = Direction::Right;
        self.next_direction = Direction::Right;
        self.score = 0;
        self.over = false;
        self.food = (15, 8);
    }

    fn turn(&mut self, direction: Direction) {
        let reverse = matches!(
            (self.direction, direction),
            (Direction::Up, Direction::Down)
                | (Direction::Down, Direction::Up)
                | (Direction::Left, Direction::Right)
                | (Direction::Right, Direction::Left)
        );
        if !reverse {
            self.next_direction = direction;
        }
    }

    fn random_food(&mut self) {
        for _ in 0..(COLS * ROWS) {
            self.seed = self
                .seed
                .wrapping_mul(1_664_525)
                .wrapping_add(1_013_904_223);
            let spot = ((self.seed >> 8) as i32 % (COLS * ROWS)).abs();
            let candidate = (spot % COLS, spot / COLS);
            if !self.body.contains(&candidate) {
                self.food = candidate;
                return;
            }
        }
    }

    fn tick(&mut self) {
        if self.over {
            return;
        }
        self.direction = self.next_direction;
        let (x, y) = self.body.front().copied().unwrap();
        let head = match self.direction {
            Direction::Up => (x, y - 1),
            Direction::Down => (x, y + 1),
            Direction::Left => (x - 1, y),
            Direction::Right => (x + 1, y),
        };
        if head.0 < 0 || head.1 < 0 || head.0 >= COLS || head.1 >= ROWS || self.body.contains(&head)
        {
            self.over = true;
            return;
        }
        self.body.push_front(head);
        if head == self.food {
            self.score += 1;
            self.random_food();
        } else {
            self.body.pop_back();
        }
    }

    fn draw(&self, surface: &mut Surface, font: &Font<'_>) {
        surface.pixels_mut().fill(BG);
        font.draw(
            surface,
            10,
            8,
            &format!("Snake   Score: {}", self.score),
            TEXT,
        );
        let ox = (surface.width() as i32 - COLS * CELL) / 2;
        let oy = 36;
        surface.fill_rect(ox - 2, oy - 2, COLS * CELL + 4, ROWS * CELL + 4, PANEL);
        surface.fill_rect(
            ox + self.food.0 * CELL,
            oy + self.food.1 * CELL,
            CELL - 1,
            CELL - 1,
            FOOD,
        );
        for (index, &(x, y)) in self.body.iter().enumerate() {
            let color = if index == 0 { 0xffdfffea } else { SNAKE };
            surface.fill_rect(ox + x * CELL, oy + y * CELL, CELL - 1, CELL - 1, color);
        }
        if self.over {
            font.draw(
                surface,
                ox + 52,
                oy + ROWS * CELL / 2 - 8,
                "GAME OVER",
                FOOD,
            );
            font.draw(
                surface,
                ox + 34,
                oy + ROWS * CELL / 2 + 14,
                "Enter to restart",
                TEXT,
            );
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::connect()?;
    let window = client.create("Snake", 380, 330, SNAKE)?;
    client.flags(window, WINDOW_RAW_INPUT | WINDOW_DEFER_CLOSE)?;
    let font = Font::builtin();
    let mut surface = Surface::new(380, 330);
    let mut game = Game::new();
    let mut last_tick = Instant::now();
    loop {
        let (width, height, minimized) = client.size(window)?;
        if minimized {
            thread::sleep(Duration::from_millis(40));
            continue;
        }
        if (width as usize, height as usize) != (surface.width(), surface.height()) {
            surface.reset(width as usize, height as usize, BG);
        }
        while let Some(Event { kind, text, .. }) = client.poll(window)? {
            if kind == 6 {
                match text.as_bytes() {
                    [27, 91, 65] | [b'w' | b'W'] => game.turn(Direction::Up),
                    [27, 91, 66] | [b's' | b'S'] => game.turn(Direction::Down),
                    [27, 91, 68] | [b'a' | b'A'] => game.turn(Direction::Left),
                    [27, 91, 67] | [b'd' | b'D'] => game.turn(Direction::Right),
                    [13] | [10] if game.over => {
                        game.reset();
                        last_tick = Instant::now();
                    }
                    [27] => {
                        let _ = client.close(window);
                        return Ok(());
                    }
                    _ => {}
                }
            } else if kind == 7 || kind == 9 {
                let _ = client.close(window);
                return Ok(());
            }
        }
        if last_tick.elapsed() >= Duration::from_millis(125) {
            game.tick();
            last_tick = Instant::now();
        }
        game.draw(&mut surface, &font);
        client.present(window, width, height, surface.pixels())?;
        thread::sleep(Duration::from_millis(16));
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("hos-snake: {error}");
    }
}
