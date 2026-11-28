"""Native normal-channel charge and drift/diffusion reference, ngspice 47.

Uses the original source/drain Poisson charge branches and independent extended
precision bisection. The actual C integral and area-charge expressions are
extracted from both evaluators. Independent finite differences are applied to
the entire coupled solve, including region selection, at each trial input.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

from hisim_hv_surface_reference import REVISION, program as surface_program

# Vgp, Vbscl, Vdseff, beta, Cox, cnst0, cnst1 (last two relative gradients).
GRADIENTS = ((1., .015, -.03, .0002), (0., .02, .91, -.00015),
             (0., .8, 0., 0.), (0., 0., 0., -.13),
             (.0001, .00002, -.00003, .000002),
             (0., 0., 0., -.0015), (0., 0., 0., .025))


def program(root, version):
    prefix = 'hsmhv' if version == 1 else 'hsmhv2'
    src = subprocess.check_output(['git', '-C', str(root), 'show',
        f'{REVISION}:src/spicelib/devices/hisimhv{version}/{prefix}eval.c'], text=True)
    start = src.index('T1 = beta / Xi0 ;')
    end = src.index('* Skip CLM and integrated charges', start)
    integral = src[start:src.rfind('/*', start, end)]
    start = src.index('* Qbu : -Qb in unit area.', end)
    start = src.index('*/', start) + 2
    end = src.index('start_of_mobility:', start)
    charge = src[start:end]
    sections = re.sub(r'/\*.*?\*/', '', integral + charge, flags=re.S)
    derivatives = set(re.findall(r'\b\w+_d(?:V\w+|T)\b', sections))
    decl = ';'.join(f'long double {name}=0.' for name in sorted(derivatives)) + ';'
    # Native Fn_CP macro contains its own scratch variables.
    start = src.index('#define Fn_CP(')
    end = re.search(r'(?m)^\s*}[^\S\n]*$', src[start:]).end() + start
    clamp = src[start:end].replace('double', 'long double').replace('sqrt(', 'sqrtl(')
    integral = integral.replace('sqrt(', 'sqrtl(')
    charge = charge.replace('sqrt(', 'sqrtl(')
    defined = set('Vgp Vbscl Vds beta beta_inv Cox cnst0 cnst1 fac1 Ps0 Psl Pds Chi Xi0 Xi0p12 Xi0p32 Xilp32 Qb0 Qn0 VgVt FD2 flg_zone Qbu Qiu Qdrat Lred Idd C_2o3 epsm10'.split())
    lhs = set(re.findall(r'\b([A-Za-z_]\w*)\s*(?:=|\*=|/=|\+=|-=)\s*[^=]', sections)) - derivatives - defined
    lhs.update(('TY', 'T4'))
    lhs -= defined | derivatives
    scalars = ','.join(f'{name}=0.' for name in sorted(lhs))
    preamble = surface_program(root, version).split('int main(void)', 1)[0]
    source_code = preamble + f'''
#define C_2o3 (2.L/3.L)
#define Fn_Pow powl
#define Fn_Max fmaxl
{clamp}
static void coupled(long double Vgp,long double Vbscl,long double Vds,long double beta,long double Cox,long double cnst0,long double cnst1,long double*out) {{
 const long double epsm10=2.220446049250313e-15L;
 long double fac1=cnst0/Cox,beta_inv=1.L/beta,sp[5],dp[5];
 solve(0,Vgp,Vbscl,0.L,beta,fac1,cnst1,sp);
 long double Ps0=sp[0],Psl=Ps0,Pds=0.,Chi=sp[1],fb=sp[2],fs01=sp[3],fs02=sp[4];
 long double Xi0=Chi<5.L?fb*fb+epsm10:Chi-1.L;
 long double Xi0p12=Chi<5.L?fb+epsm10:fb;
 long double Xi0p32=Chi<5.L?fb*fb*fb+epsm10:Xi0*Xi0p12;
 long double Qb0=cnst0*Xi0p12,Qn0=cnst0*fs01/(fs02+Xi0p12),VgVt=Qn0/Cox;
 int flg_zone=Chi<3.L?1:(Chi<5.L?2:3);
 long double FD2=0.,Qbu=Qb0,Qiu=Qn0,Qdrat=.5L,Lred=0.,Idd=0.;
 if(flg_zone==3)FD2=1.;else if(flg_zone==2){{long double tx=(Chi-3.L)/2.L;FD2=tx*tx*tx*(10.L+tx*(-15.L+6.L*tx));}}
 if (VgVt<=1e-12L) {{ flg_zone=4;Qiu=0.; }} else {{
 solve(1,Vgp,Vbscl,Vds,beta,fac1,cnst1,dp);
 Psl=dp[0];Pds=fmaxl(0.,Psl-Ps0);long double Xilp32=dp[1]<5.L?dp[2]*dp[2]*dp[2]+epsm10:(dp[1]-1.L)*dp[2];
 {decl}
 long double {scalars};
 {integral}
 if (flg_zone!=1) {{ {charge} }}
 }}
 long double values[]={{flg_zone,Ps0,Psl,Pds,Qb0,Qn0,VgVt,Idd,Qbu,Qiu,Qdrat,FD2}};
 for(int j=0;j<12;j++)out[j]=values[j];
}}
int main(void) {{ long double x[8];while(1){{
 for(int i=0;i<8;i++){{char token[128];if(scanf("%127s",token)!=1)return 0;x[i]=strtoflt128(token,0);}}
 long double state[5],values[12];charge(0,x[1]+x[0]/x[3],x[1],0.L,x[3],x[6],state);
 long double gate=state[0]+x[5]/x[4]*state[4];
 coupled(gate,x[1],x[2],x[3],x[4],x[5],x[6],values);
 emit(gate);for(int j=0;j<12;j++){{putchar(32);emit(values[j]);}}
 long double grads[7][4]={{{','.join('{'+','.join(map(str,g))+'}' for g in GRADIENTS)}}};
 long double args[7]={{gate,x[1],x[2],x[3],x[4],x[5],x[6]}};
 for(int i=5;i<7;i++)for(int axis=0;axis<4;axis++)grads[i][axis]*=args[i];
 for(int axis=0;axis<4;axis++){{long double h=axis==3?1e-3L:1e-5L,lo[7],hi[7],a[12],b[12];
 for(int j=0;j<7;j++){{lo[j]=args[j]-h*grads[j][axis];hi[j]=args[j]+h*grads[j][axis];}}
 coupled(hi[0],hi[1],hi[2],hi[3],hi[4],hi[5],hi[6],a);
 if(axis==1 && x[2]==0.L){{
  for(int j=0;j<7;j++)hi[j]=args[j]+2*h*grads[j][axis];
  coupled(hi[0],hi[1],hi[2],hi[3],hi[4],hi[5],hi[6],b);
  for(int j=1;j<12;j++){{putchar(32);emit((4*a[j]-b[j]-3*values[j])/(2*h));}}
 }}else{{
  coupled(lo[0],lo[1],lo[2],lo[3],lo[4],lo[5],lo[6],b);
  for(int j=1;j<12;j++){{putchar(32);emit((a[j]-b[j])/(2*h));}}
 }}
 }}putchar('\\n');
}}}}
'''

    # Resolve tiny surface differences independently, at binary128 precision.
    source_code = source_code.replace('long double', '__float128').replace('sqrtl(', 'sqrtq(').replace('expl(', 'expq(').replace('powl', 'powq').replace('fmaxl', 'fmaxq').replace('i<100;i++', 'i<160;i++')
    source_code = re.sub(r'(?<![\w.])(?:\d+\.\d*|\.\d+|\d+[eE][+-]?\d+)(?:[eE][+-]?\d+)?[lL]?(?![\w.])', lambda m: m.group().rstrip('lL') + 'Q', source_code)
    return '#include <stdio.h>\n#include <quadmath.h>\nstatic void emit(__float128 x){char buf[128];quadmath_snprintf(buf,sizeof buf,"%.35Qg",x);fputs(buf,stdout);}\n' + source_code


def main():
    root, out = map(Path, sys.argv[1:])
    cases = list(product((.1, 2.8, 3.2, 4.8, 5.2, 12., 25., 40.),
                         (-.6, -.1, .15), (0., .01, .3, 1.5), (30., 42.),
                         (.003, .007), (.0003,), (1e-14, 1e-8), (0.,)))
    with tempfile.TemporaryDirectory(prefix='hisim-charge-oracle-') as tmp, out.open('w') as f:
        f.write('# family chi_seed Vbscl Vdseff beta Cox cnst0 cnst1; Vgp region; 11 results and 4 groups of 11 derivatives\n')
        for version in (1, 2):
            c, exe = Path(tmp)/'oracle.c', Path(tmp)/'oracle'
            c.write_text(program(root, version))
            subprocess.run(['sccache', 'cc', '-std=c11', '-O2', str(c), '-lm', '-lquadmath', '-o', str(exe)], check=True)
            result = subprocess.run([str(exe)], input=''.join(' '.join(map(str,row))+'\n' for row in cases), text=True, capture_output=True, check=True)
            rows = result.stdout.splitlines()
            assert len(rows) == len(cases)
            for case,row in zip(cases,rows):
                assert all(math.isfinite(float(x)) for x in row.split()), (version,case,row)
                f.write(' '.join(map(str,(version,*case[:-1])))+' '+row+'\n')

if __name__ == '__main__':
    main()
