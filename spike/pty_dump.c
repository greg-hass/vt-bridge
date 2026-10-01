// Spike: run a command in a PTY, feed output to libghostty-vt, dump the screen as plain text.
#define _GNU_SOURCE
#include <pty.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/wait.h>
#include <ghostty/vt.h>

static void write_pty(GhosttyTerminal t, void *ud, const uint8_t *d, size_t n) {
  (void)t; write(*(int *)ud, d, n);   // terminal replies (cursor pos, DA...) go back to the child
}

int main(int argc, char **argv) {
  if (argc < 2) { fprintf(stderr, "usage: %s cmd [args]\n", argv[0]); return 2; }
  const int COLS = 100, ROWS = 30;
  struct winsize ws = { .ws_row = ROWS, .ws_col = COLS };
  int fd; pid_t pid = forkpty(&fd, NULL, NULL, &ws);
  if (pid == 0) { setenv("TERM", "xterm-256color", 1); execvp(argv[1], argv + 1); _exit(127); }

  GhosttyTerminal term; ghostty_terminal_new(NULL, &term, COLS, ROWS);
  ghostty_terminal_set(term, GHOSTTY_TERMINAL_OPT_USERDATA, &fd);
  ghostty_terminal_set(term, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (void *)write_pty);

  uint8_t buf[65536]; int limit = 40;           // ~4 s max
  while (limit--) {
    struct pollfd p = { fd, POLLIN, 0 };
    if (poll(&p, 1, 100) > 0) {
      ssize_t n = read(fd, buf, sizeof buf);
      if (n <= 0) break;
      ghostty_terminal_vt_write(term, buf, n);
    } else if (waitpid(pid, NULL, WNOHANG) == pid) break;
  }
  kill(pid, 9);

  GhosttyRenderState rs; ghostty_render_state_new(NULL, &rs);
  ghostty_render_state_update(rs, term);
  GhosttyRenderStateRowIterator it; ghostty_render_state_row_iterator_new(NULL, &it);
  ghostty_render_state_get(rs, GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &it);
  GhosttyRenderStateRowCells cells; ghostty_render_state_row_cells_new(NULL, &cells);
  int styled = 0, y = 0;
  while (ghostty_render_state_row_iterator_next(it)) {
    ghostty_render_state_row_get(it, GHOSTTY_RENDER_STATE_ROW_DATA_CELLS, &cells);
    char line[1024]; int len = 0;
    while (ghostty_render_state_row_cells_next(cells)) {
      uint32_t gl = 0; ghostty_render_state_row_cells_get(cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN, &gl);
      if (!gl) { line[len++] = ' '; continue; }
      uint32_t cps[8]; ghostty_render_state_row_cells_get(cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, cps);
      uint32_t c = cps[0];
      if (c < 0x80) line[len++] = c;
      else { // minimal utf-8 encode
        if (c < 0x800) { line[len++] = 0xC0 | c >> 6; line[len++] = 0x80 | (c & 63); }
        else if (c < 0x10000) { line[len++] = 0xE0 | c >> 12; line[len++] = 0x80 | (c >> 6 & 63); line[len++] = 0x80 | (c & 63); }
        else { line[len++] = 0xF0 | c >> 18; line[len++] = 0x80 | (c >> 12 & 63); line[len++] = 0x80 | (c >> 6 & 63); line[len++] = 0x80 | (c & 63); }
      }
      GhosttyStyle st = GHOSTTY_INIT_SIZED(GhosttyStyle);
      ghostty_render_state_row_cells_get(cells, GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &st);
      if (st.fg_color.tag != GHOSTTY_STYLE_COLOR_NONE || st.bold) styled++;
    }
    while (len && line[len-1] == ' ') len--;
    line[len] = 0; printf("%2d|%s\n", y++, line);
  }
  printf("-- styled cells: %d\n", styled);
  return 0;
}
