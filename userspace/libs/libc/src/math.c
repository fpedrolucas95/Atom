/*
 * math.c — AtomOS libc basic math functions.
 *
 * Uses x87 / SSE2 instructions directly via GCC built-ins where possible.
 * The compiler-provided __builtin_* variants are used for most functions
 * to ensure correct IEEE 754 semantics without pulling in libm.
 *
 * Transcendental functions (sin, cos, …) use the x87 FPU instructions
 * which are available on all x86_64 processors.
 */

#include <math.h>

/* =========================================================================
 * Rounding and absolute value
 * ========================================================================= */

double fabs(double x)  { return __builtin_fabs(x);  }
float  fabsf(float x)  { return __builtin_fabsf(x); }

double floor(double x) { return __builtin_floor(x); }
float  floorf(float x) { return __builtin_floorf(x); }

double ceil(double x)  { return __builtin_ceil(x);  }
float  ceilf(float x)  { return __builtin_ceilf(x); }

double round(double x) { return __builtin_round(x); }
double trunc(double x) { return __builtin_trunc(x); }

/* =========================================================================
 * Square root
 * ========================================================================= */

double sqrt(double x)
{
    double r;
    __asm__ volatile ("sqrtsd %1, %0" : "=x"(r) : "x"(x));
    return r;
}

float sqrtf(float x)
{
    float r;
    __asm__ volatile ("sqrtss %1, %0" : "=x"(r) : "x"(x));
    return r;
}

/* =========================================================================
 * Exponentiation and logarithms — using x87 FPU
 * ========================================================================= */

double log(double x)
{
    /* log(x) = log2(x) / log2(e) = fyl2x(x, 1) * ln(2) */
    double r;
    __asm__ volatile (
        "fldln2\n\t"
        "fldl %1\n\t"
        "fyl2x\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(x)
    );
    return r;
}

double log2(double x)
{
    double r;
    __asm__ volatile (
        "fld1\n\t"
        "fldl %1\n\t"
        "fyl2x\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(x)
    );
    return r;
}

double log10(double x)
{
    double r;
    __asm__ volatile (
        "fldlg2\n\t"
        "fldl %1\n\t"
        "fyl2x\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(x)
    );
    return r;
}

double exp(double x)
{
    /* e^x = 2^(x * log2(e)) */
    double r;
    __asm__ volatile (
        "fldl %1\n\t"
        "fldl2e\n\t"
        "fmulp\n\t"
        /* Now compute 2^st0 via f2xm1 + 1 for fractional part */
        "fld %%st(0)\n\t"
        "frndint\n\t"
        "fsubr %%st, %%st(1)\n\t"
        "fxch\n\t"
        "f2xm1\n\t"
        "fld1\n\t"
        "faddp\n\t"
        "fscale\n\t"
        "fstp %%st(1)\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(x) : "st", "st(1)"
    );
    return r;
}

/* =========================================================================
 * Power
 * ========================================================================= */

double pow(double base, double exp_val)
{
    if (exp_val == 0.0) return 1.0;
    if (base == 0.0)    return 0.0;
    if (base < 0.0) {
        /* Handle negative base with integer exponent */
        long long iexp = (long long)exp_val;
        if ((double)iexp == exp_val) {
            double r = pow(-base, exp_val);
            return (iexp & 1) ? -r : r;
        }
        return NAN;
    }
    return exp(exp_val * log(base));
}

/* =========================================================================
 * Modulo
 * ========================================================================= */

double fmod(double x, double y)
{
    double r;
    __asm__ volatile (
        "fldl %2\n\t"
        "fldl %1\n\t"
        "1:\n\t"
        "fprem\n\t"
        "fnstsw %%ax\n\t"
        "testb $4, %%ah\n\t"
        "jnz 1b\n\t"
        "fstpl %0\n\t"
        "fstp %%st(0)"
        : "=m"(r) : "m"(x), "m"(y) : "ax"
    );
    return r;
}

double modf(double x, double *iptr)
{
    *iptr = trunc(x);
    return x - *iptr;
}

/* =========================================================================
 * Ldexp / frexp
 * ========================================================================= */

double ldexp(double x, int exp_n)
{
    double r;
    double scale;
    __asm__ volatile (
        "fildl %1\n\t"
        "fld1\n\t"
        "fscale\n\t"
        "fstp %%st(1)\n\t"
        "fstpl %0"
        : "=m"(scale) : "m"(exp_n)
    );
    r = x * scale;
    return r;
}

double frexp(double x, int *exp_out)
{
    if (x == 0.0) { *exp_out = 0; return 0.0; }
    if (isinf(x) || isnan(x)) { *exp_out = 0; return x; }
    int e = 0;
    double v = fabs(x);
    while (v >= 1.0) { v /= 2.0; e++; }
    while (v < 0.5)  { v *= 2.0; e--; }
    *exp_out = e;
    return (x < 0.0) ? -v : v;
}

/* =========================================================================
 * Trigonometric functions — x87 FPU
 * ========================================================================= */

double sin(double x)
{
    double r;
    __asm__ volatile ("fldl %1; fsin; fstpl %0" : "=m"(r) : "m"(x));
    return r;
}

double cos(double x)
{
    double r;
    __asm__ volatile ("fldl %1; fcos; fstpl %0" : "=m"(r) : "m"(x));
    return r;
}

double tan(double x)
{
    double r;
    __asm__ volatile (
        "fldl %1\n\t"
        "fptan\n\t"
        "fstp %%st(0)\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(x)
    );
    return r;
}

double atan(double x)
{
    double r;
    __asm__ volatile (
        "fld1\n\t"
        "fldl %1\n\t"
        "fpatan\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(x)
    );
    return r;
}

double atan2(double y, double x)
{
    double r;
    __asm__ volatile (
        "fldl %2\n\t"
        "fldl %1\n\t"
        "fpatan\n\t"
        "fstpl %0"
        : "=m"(r) : "m"(y), "m"(x)
    );
    return r;
}

double asin(double x)
{
    /* asin(x) = atan2(x, sqrt(1 - x*x)) */
    return atan2(x, sqrt(1.0 - x * x));
}

double acos(double x)
{
    /* acos(x) = atan2(sqrt(1 - x*x), x) */
    return atan2(sqrt(1.0 - x * x), x);
}

/* =========================================================================
 * Hyperbolic functions
 * ========================================================================= */

double sinh(double x) { return (exp(x) - exp(-x)) / 2.0; }
double cosh(double x) { return (exp(x) + exp(-x)) / 2.0; }
double tanh(double x)
{
    double ex  = exp(x);
    double enx = exp(-x);
    return (ex - enx) / (ex + enx);
}
