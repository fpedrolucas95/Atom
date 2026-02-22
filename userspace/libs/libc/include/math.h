#ifndef _MATH_H
#define _MATH_H

#define M_E        2.71828182845904523536
#define M_LOG2E    1.44269504088896340736
#define M_LOG10E   0.43429448190325182765
#define M_LN2      0.69314718055994530941
#define M_LN10     2.30258509299404568402
#define M_PI       3.14159265358979323846
#define M_PI_2     1.57079632679489661923
#define M_PI_4     0.78539816339744830962
#define M_1_PI     0.31830988618379067154
#define M_2_PI     0.63661977236758134308
#define M_SQRT2    1.41421356237309504880
#define M_SQRT1_2  0.70710678118654752440

#define HUGE_VAL   (__builtin_huge_val())
#define HUGE_VALF  (__builtin_huge_valf())
#define INFINITY   (__builtin_inff())
#define NAN        (__builtin_nanf(""))

#define isinf(x)   __builtin_isinf(x)
#define isnan(x)   __builtin_isnan(x)
#define isfinite(x) __builtin_isfinite(x)

double fabs(double x);
double floor(double x);
double ceil(double x);
double round(double x);
double trunc(double x);
double sqrt(double x);
double pow(double base, double exp);
double fmod(double x, double y);
double modf(double x, double *iptr);
double log(double x);
double log2(double x);
double log10(double x);
double exp(double x);
double sin(double x);
double cos(double x);
double tan(double x);
double asin(double x);
double acos(double x);
double atan(double x);
double atan2(double y, double x);
double sinh(double x);
double cosh(double x);
double tanh(double x);
double ldexp(double x, int exp);
double frexp(double x, int *exp);

float fabsf(float x);
float sqrtf(float x);
float floorf(float x);
float ceilf(float x);

#endif /* _MATH_H */
