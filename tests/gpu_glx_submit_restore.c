//# link_flags: -lGL -lX11
//# gpu: yes
//# env: MESA_LOADER_DRIVER_OVERRIDE=zink
//# max_restores: 3
// TODO(ai-review): generated, not yet audited
//
// GL checkpoint/restore through the guest graphics stack (zink over the
// gfxstream guest ICD). The GL driver submits work asynchronously from
// its own worker threads; a frame-boundary checkpoint can park such a
// worker mid-flight in the encoder ring. The restore rewinds the shared
// ring state, but the notification that would make the host consume the
// pending bytes does not survive the round trip, so the worker spins
// forever and no further frames are rendered.
//
// RED (unquiesced checkpoint): the first restore deadlocks; frames stop
// printing and the harness times out.
// GREEN (checkpoint quiesces the driver): frames keep printing through
// every restore; the harness ends the run after max_restores.
//
// Each frame encodes well over the encoder ring size in command traffic
// (texture and buffer uploads plus draw calls) so the driver's submit
// worker is essentially always mid-transfer at the frame boundary.
//
// Needs an X display and the gfxstream guest ICD (gpu: yes). Renders to a
// GLX pbuffer and never presents: the X11 present/swapchain path across a
// restore is a separate, known-open problem this test deliberately avoids.

#include <assert.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <GL/glx.h>
#include <GL/glxext.h>
#include "testlib.h"

static GLuint (*p_glCreateShader)(GLenum);
static void (*p_glShaderSource)(GLuint, GLsizei, const GLchar *const *, const GLint *);
static void (*p_glCompileShader)(GLuint);
static void (*p_glGetShaderiv)(GLuint, GLenum, GLint *);
static GLuint (*p_glCreateProgram)(void);
static void (*p_glAttachShader)(GLuint, GLuint);
static void (*p_glLinkProgram)(GLuint);
static void (*p_glGetProgramiv)(GLuint, GLenum, GLint *);
static void (*p_glUseProgram)(GLuint);
static GLint (*p_glGetUniformLocation)(GLuint, const GLchar *);
static void (*p_glUniform1f)(GLint, GLfloat);
static void (*p_glGenVertexArrays)(GLsizei, GLuint *);
static void (*p_glBindVertexArray)(GLuint);
static void (*p_glGenBuffers)(GLsizei, GLuint *);
static void (*p_glBindBuffer)(GLenum, GLuint);
static void (*p_glBufferData)(GLenum, GLsizeiptr, const void *, GLenum);

static void *gpa(const char *name) {
    return (void *)glXGetProcAddressARB((const GLubyte *)name);
}

static GLuint make_shader(GLenum type, const char *src) {
    GLuint sh = p_glCreateShader(type);
    p_glShaderSource(sh, 1, &src, NULL);
    p_glCompileShader(sh);
    GLint ok = 0;
    p_glGetShaderiv(sh, GL_COMPILE_STATUS, &ok);
    assert(ok && "shader compile failed");
    return sh;
}

enum { TEX_SIZE = 512 * 512 * 4, BUF_SIZE = 512 * 1024 };
static unsigned char texdata[TEX_SIZE];
static unsigned char bufdata[BUF_SIZE];

int main(void) {
    Display *dpy = XOpenDisplay(NULL);
    assert(dpy && "no X display");

    int fb_attr[] = {GLX_DRAWABLE_TYPE, GLX_PBUFFER_BIT,
                     GLX_RENDER_TYPE,   GLX_RGBA_BIT,
                     GLX_DOUBLEBUFFER,  True,
                     GLX_RED_SIZE,      8,
                     GLX_GREEN_SIZE,    8,
                     GLX_BLUE_SIZE,     8,
                     GLX_DEPTH_SIZE,    24,
                     None};
    int nfb = 0;
    GLXFBConfig *fbc = glXChooseFBConfig(dpy, DefaultScreen(dpy), fb_attr, &nfb);
    assert(fbc && nfb > 0 && "no fbconfig");

    GLXContext (*create_ctx_attribs)(Display *, GLXFBConfig, GLXContext, Bool,
                                     const int *) = gpa("glXCreateContextAttribsARB");
    assert(create_ctx_attribs);
    int ctx_attr[] = {GLX_CONTEXT_MAJOR_VERSION_ARB, 4,
                      GLX_CONTEXT_MINOR_VERSION_ARB, 1,
                      GLX_CONTEXT_PROFILE_MASK_ARB, GLX_CONTEXT_CORE_PROFILE_BIT_ARB,
                      None};
    GLXContext ctx = create_ctx_attribs(dpy, fbc[0], NULL, True, ctx_attr);
    assert(ctx && "context creation failed");

    // A pbuffer keeps the swapchain out of the test: presentation across a
    // restore is a separate, known-open problem (X11 present desync), and
    // this test targets the submit ring.
    int pb_attr[] = {GLX_PBUFFER_WIDTH, 640, GLX_PBUFFER_HEIGHT, 480, None};
    GLXPbuffer pbuf = glXCreatePbuffer(dpy, fbc[0], pb_attr);
    assert(pbuf && "pbuffer creation failed");
    assert(glXMakeCurrent(dpy, pbuf, ctx) && "make current failed");

    p_glCreateShader = gpa("glCreateShader");
    p_glShaderSource = gpa("glShaderSource");
    p_glCompileShader = gpa("glCompileShader");
    p_glGetShaderiv = gpa("glGetShaderiv");
    p_glCreateProgram = gpa("glCreateProgram");
    p_glAttachShader = gpa("glAttachShader");
    p_glLinkProgram = gpa("glLinkProgram");
    p_glGetProgramiv = gpa("glGetProgramiv");
    p_glUseProgram = gpa("glUseProgram");
    p_glGetUniformLocation = gpa("glGetUniformLocation");
    p_glUniform1f = gpa("glUniform1f");
    p_glGenVertexArrays = gpa("glGenVertexArrays");
    p_glBindVertexArray = gpa("glBindVertexArray");
    p_glGenBuffers = gpa("glGenBuffers");
    p_glBindBuffer = gpa("glBindBuffer");
    p_glBufferData = gpa("glBufferData");
    assert(p_glCreateShader && p_glUseProgram && p_glBufferData);

    const GLubyte *renderer = glGetString(GL_RENDERER);
    tprintf("zink=%d\n", renderer && strstr((const char *)renderer, "zink") != NULL);

    GLuint vs = make_shader(GL_VERTEX_SHADER, "#version 410 core\nuniform float off;\nvoid main() {}\n");
    GLuint fs = make_shader(GL_FRAGMENT_SHADER, "#version 410 core\nvoid main() {}\n");
    GLuint prog = p_glCreateProgram();
    p_glAttachShader(prog, vs);
    p_glAttachShader(prog, fs);
    p_glLinkProgram(prog);
    GLint linked = 0;
    p_glGetProgramiv(prog, GL_LINK_STATUS, &linked);
    assert(linked && "program link failed");
    p_glUseProgram(prog);
    GLint u_off = p_glGetUniformLocation(prog, "off");

    GLuint vao;
    p_glGenVertexArrays(1, &vao);
    p_glBindVertexArray(vao);
    GLuint vbo;
    p_glGenBuffers(1, &vbo);
    p_glBindBuffer(GL_ARRAY_BUFFER, vbo);

    GLuint tex;
    glGenTextures(1, &tex);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);

    for (int frame = 0;; frame++) {
        texdata[0] = (unsigned char)frame;
        bufdata[0] = (unsigned char)frame;

        glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, 512, 512, 0, GL_RGBA,
                     GL_UNSIGNED_BYTE, texdata);
        p_glBufferData(GL_ARRAY_BUFFER, BUF_SIZE, bufdata, GL_STREAM_DRAW);

        glClearColor(0.1f, 0.2f, 0.3f, 1.0f);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        for (int i = 0; i < 400; i++) {
            p_glUniform1f(u_off, (float)i + (float)frame);
            glDrawArrays(GL_TRIANGLES, 0, 3);
        }

        tprintf("frame=%d\n", frame);
        if (frame == 3 && access("/tmp/glx-hold", F_OK) == 0) {
            // opt-in debug gate: hold before the checkpoint/restore window
            // (touch /tmp/glx-hold, then release with /tmp/glx-go) so a
            // tracer can be attached to the live process
            while (access("/tmp/glx-go", F_OK) != 0) usleep(1000);
        }
        if (frame == 5) valo_checkpoint();
        if (frame == 10) valo_restore();
        glFlush();
    }
}
